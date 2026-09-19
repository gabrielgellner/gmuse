//! The artist → album → track tree, over the model — issue #84.
//!
//! `library.rs` builds this shape from a `Scan`, keyed by an arena index and
//! grouped by a heuristic. This builds the same shape from a [`Librarian`],
//! keyed by identity and grouped by facts the model states.
//!
//! Two things change, and neither is cosmetic.
//!
//! **Artist grouping asks for a credit.** `Track::file_under` picks album
//! artist, else artist, else "Unknown" — which is guessing at something the
//! model says outright. A guest credit on one track no longer risks splitting
//! an album off from the artist it belongs to, because the album's own credit
//! is asked first rather than inferred from a tag that might be missing on
//! exactly the track being looked at.
//!
//! **The album level is a release group.** So the reissue trap stops
//! mattering: a 2015 pressing of a 1982 album files under the album, and
//! ordering by date reads the group's `first_released` and gets 1982 (#59).
//!
//! ## Why nothing browses this yet
//!
//! **Both of those ask a recording a question only a file can answer**, and
//! that is not a gap in the wiring — it is the thing #78 exists to settle.
//!
//! A recording appears on every release that holds it; a file sits on exactly
//! one. `Librarian::files_under` and `album_of` reach for
//! `tracks_of_recording(..).first()`, so for anything held by a compilation
//! *and* its original album, the answer is whichever placement sorts first.
//!
//! Measured against the real library, replacing `library.rs`'s tag-derived
//! tree with this one moved tracks between **60 albums**: `Who Made Who` is an
//! AC/DC compilation, so the file at `Back in Black/02 Hells Bells.mp3` was
//! filed under `Who Made Who` — correct about the recording and wrong about
//! the copy on disk.
//!
//! Declining instead of guessing is not the fix either. Falling back to the
//! recording's own credit files `Islands in the Stream` under *Dolly Parton
//! Duet with Kenny Rogers*, which is precisely the split `file_under`'s
//! album-artist preference exists to prevent: **197 artists became 207.**
//!
//! So a browsable row has to name the *placement* it means, and that is #78.
//! Until then `library.rs` keeps grouping by the file's own tags, which are
//! the ones actually true of the copy in hand.

use std::collections::BTreeMap;

use crate::model::entity::ReleaseGroup;
use crate::model::id::Id;
use crate::model::seam::{Librarian, Listing};
use crate::model::sortkey::{SortKey, sort_key};
use crate::model::spine::PartialDate;
use crate::query::format::Field;

/// One album under one artist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Album {
    pub group: Option<Id<ReleaseGroup>>,
    pub title: String,
    /// When the **album** first appeared, not when this pressing did.
    pub first_released: Option<PartialDate>,
    /// In disc-then-track order, as the tags claim it.
    ///
    /// **One entry per copy**, not per recording — issue #84. Two files are
    /// two things you hold, and each knows which slot it sits in, so a song on
    /// an album and on a compilation appears under both rather than under
    /// whichever placement sorted first.
    pub tracks: Vec<Listing>,
}

/// The tree a browser walks.
#[derive(Debug, Default)]
pub struct Tree {
    artists: BTreeMap<SortKey, BTreeMap<SortKey, Album>>,
    /// What to **show** for each artist key.
    ///
    /// The tree is keyed by a sort key — case folded, marks flattened, article
    /// moved — which is right for ordering and unreadable on screen. Without
    /// this a renderer has to reach back for a recording under the node just
    /// to recover the string, which is how a projection that exists to answer
    /// questions ends up unable to answer the simplest one.
    artist_names: BTreeMap<SortKey, String>,
}

/// What a row says for one level, as strings.
///
/// **A set, because a level may be multi-valued** — a row has one artist and
/// many genres. `Field::Facet` is the case that matters; everything else
/// yields one value or none.
///
/// A missing value gets a stand-in rather than being dropped: a track with no
/// genre is still a track you hold, and filing it nowhere is how a library
/// grows holes nobody notices.
fn values_of(
    l: &Librarian,
    level: crate::shelf::Level,
    subject: &crate::model::rows::Subject<'_>,
    slot: Option<crate::model::id::Id<crate::model::entity::Track>>,
) -> Vec<String> {
    let field = level.field;
    // **An artist level climbs the moniker graph, not the codelist** (#174).
    // Asked of the librarian rather than resolved here, because the answer is
    // a credit line and `line_of` is the one place that decides how one reads
    // — a tree that joined names itself would be a second such place.
    //
    // Asked of the **slot** for the reason `files_under` is (#84): a recording
    // is on every release that holds it, so asking the recording files a
    // compilation track under whichever release sorted first.
    if let (Some(at), true) = (level.at, artist_shaped(field))
        && let Some(recording) = subject.recording
    {
        let line = l.filed_under_at(recording, slot, at);
        return vec![if line.is_empty() {
            crate::model::seam::UNKNOWN_ARTIST.to_string()
        } else {
            line
        }];
    }
    // **Two fields ask the model rather than the tags, and the tree wants the
    // model's answer** (#84). `album=` in a filter deliberately asks what the
    // *tag* says — `!album?` has to stay meaningful — but a tree grouping by
    // album must group two pressings of one record together, which only the
    // release group can say. Same for who it files under.
    let held =
        crate::query::filter::texts_of(field, subject, &crate::query::filter::Env::default());
    if held.is_empty() {
        return vec![stand_in(field)];
    }
    // **A depth turns each value into its ancestor at that level** (#79).
    // Deduplicated, because two genres under one broad term are one node: a
    // row tagged `Techno; House` belongs under `Electronic` once.
    let Some(at) = level.at else { return held };
    let mut out: Vec<String> = Vec::new();
    for value in held {
        // **Every ancestor at that level, deduplicated.** Two genres under one
        // broad term are one node — a row tagged `Techno; House` belongs under
        // `Electronic` once — and one genre under two broad terms is two,
        // because the dimension is many-valued and its counts do not sum.
        for up in l.vocabulary.at_level(&value, at) {
            if !out.contains(&up) {
                out.push(up);
            }
        }
    }
    out
}

/// Whether a field names an artist, and so has the moniker graph to climb.
///
/// The same three [`crate::sort`] calls artist-shaped for its own reasons, and
/// for the same reason: they are the fields whose values are people.
fn artist_shaped(field: Field) -> bool {
    matches!(field, Field::Artist | Field::AlbumArtist | Field::FileUnder)
}

/// What to show where a level has no value.
fn stand_in(field: Field) -> String {
    match field {
        Field::Album => NO_ALBUM.to_string(),
        _ => NOTHING_SAID.to_string(),
    }
}

/// Shown for a recording that belongs to no album.
pub const NO_ALBUM: &str = "No Album";

/// Shown where a level has no value — a row with no genre under `levels genre`.
///
/// **`pub` so a client can tell the stand-in from a term**: it is drawn as a node like
/// any other, and `K` over it has to say "nothing is said" rather than look for a genre
/// called that.
pub const NOTHING_SAID: &str = "Nothing said";

impl Tree {
    /// Build the tree a library describes, grouped by two chosen fields.
    ///
    /// **`levels` is a value, not a shape somebody hardcoded** (#97). Views 1
    /// and 2 were one set drawn two ways, and this is the switch that says
    /// which — `[artist, album]` is the tree as it was, and `[genre, album]`
    /// is a grouping that could not be expressed at all before.
    ///
    /// ## A level may be multi-valued, and that is the hazard
    ///
    /// A row has one artist and many genres. Grouping by genre therefore
    /// places a copy under **each** of its genres, which is right — you want
    /// a techno track under `techno` and under `electronic` — and it means a
    /// copy appears more than once in the tree.
    ///
    /// `tree-view-design.md` §6 names the three call sites that must know:
    /// counting has to be distinct, enqueueing has to dedup, and revealing
    /// has to highlight every place it sits. Without that "a subtree reports
    /// 42 tracks and enqueues 37".
    /// ## It groups what it was given, not what the library holds
    ///
    /// **`members` is the set being drawn** (#121). This used to build the
    /// whole library and let the caller filter afterwards, so narrowing to 786
    /// rows cost exactly what drawing all 6,485 did — every `:view` paid for
    /// the whole shelf regardless of what matched. A recording with no copy is
    /// not in any member set, which is the same answer the caller's filter
    /// gave.
    pub fn grouped(
        l: &Librarian,
        levels: (crate::shelf::Level, crate::shelf::Level),
        smart_artist_sort: bool,
        members: &std::collections::HashSet<crate::model::rows::CopyId>,
    ) -> Self {
        let mut by_outer: BTreeMap<SortKey, BTreeMap<SortKey, Album>> = BTreeMap::new();
        let mut outer_names: BTreeMap<SortKey, String> = BTreeMap::new();

        // **Iterate what is being drawn, not the library.** Walking every
        // recording and asking `listings` for each allocated a `Vec` per
        // recording whether or not any of it was wanted — a floor of ~100ms on
        // 6,485 rows that no amount of narrowing could get under. A member is
        // a copy, and a copy names its recording, so the listing is built
        // rather than looked up.
        for row in members {
            let Some(recording) = crate::model::rows::Listing::recording_of(l, *row) else {
                continue;
            };
            // The same question `row_of` was being called to answer — and it
            // built and threw away a whole `Row`, title clone included, to
            // say so.
            if !l.spine.recordings.contains_key(&recording) {
                continue;
            }
            {
                let listing = Listing {
                    recording,
                    row: *row,
                    // **The copy filling this row**, which may be none (#155).
                    copy: l.shown_copy(*row),
                };
                let slot = row.slot();
                let group = l.group_of(slot);
                // Asked of the **row**, which is the appearance.
                // `Listing::subject` is gone (#261) — it handed back a borrow into the fold.
                // This side holds one, so it builds the subject itself.
                let at = crate::model::rows::Listing::playable(l, *row).map(|p| p.locator);
                let Some(subject) = l.describe(*row).map(|listed| crate::model::rows::Subject {
                    row: listed,
                    locator: at.as_deref(),
                    recording: crate::model::rows::Listing::recording_of(l, *row),
                }) else {
                    continue;
                };
                let inner_values = values_of(l, levels.1, &subject, slot);
                // The inner level takes the first value it has: a sub-node is
                // a place *within* a group, and splitting one row across two
                // sub-nodes of the same group would be a duplicate nobody
                // asked for.
                let inner = inner_values.first().cloned().unwrap_or_default();
                let inner_key = sort_key(&inner, false);

                // **Every value, at the outer level.** This is where the
                // multi-valued case lives, and it is deliberate.
                for value in values_of(l, levels.0, &subject, slot) {
                    let key = sort_key(
                        &value,
                        smart_artist_sort && levels.0.field == Field::FileUnder,
                    );
                    outer_names.entry(key.clone()).or_insert(value);
                    by_outer
                        .entry(key)
                        .or_default()
                        .entry(inner_key.clone())
                        .or_insert_with(|| Album {
                            group,
                            title: inner.clone(),
                            first_released: group
                                .and_then(|g| l.spine.groups.get(&g))
                                .and_then(|g| g.first_released),
                            tracks: Vec::new(),
                        })
                        .tracks
                        .push(listing);
                }
            }
        }
        sort_within(l, &mut by_outer);
        Tree {
            artists: by_outer,
            artist_names: outer_names,
        }
    }

    // **`Tree::of` lived here and is gone** (#97).
    //
    // It built artist-then-album, which is what `grouped` does when nobody
    // says otherwise — so the pair is a *default* now rather than a shape
    // somebody hardcoded, and that is the whole of what this issue is about.
}

/// Order the tracks inside every node, one rule for both groupings.
fn sort_within(l: &Librarian, by_outer: &mut BTreeMap<SortKey, BTreeMap<SortKey, Album>>) {
    {
        for albums in by_outer.values_mut() {
            for album in albums.values_mut() {
                // **Position first, then the title, then the id** (#116).
                //
                // Position alone is not a total order and this library proves
                // it: AC/DC's *High Voltage* is two different albums of that
                // name in one folder, so six pairs of tracks share a number.
                // Left to position alone the two orderings that existed here
                // disagreed about twelve rows, which is a library that looks
                // different depending on which projection drew it.
                // **Cached, because the key is expensive and `sort_by` asks
                // for it once per comparison** (#121). `row_of` builds a whole
                // `Row` — cloning the title, resolving the album and who it
                // files under — and lowercasing allocates again. Called
                // O(n log n) times that was 1.03s of a 1.26s tree build on a
                // 6,485-row library, four fifths of the cost of drawing it.
                // `sort_by_cached_key` asks once per row.
                album.tracks.sort_by_cached_key(|c| {
                    // **The title straight off the spine.** `row_of` returns
                    // it, and also resolves the album and who the recording
                    // files under — none of which this key uses.
                    let title = l
                        .spine
                        .recordings
                        .get(&c.recording)
                        .map(|r| r.title.to_lowercase())
                        .unwrap_or_default();
                    // **The row's own slot, not the copy's** (#155). This
                    // was `slot_of(c.copy)`, the same mistake `row_of_via`
                    // had: a row the library holds no file for has no copy,
                    // so `place_of` answered `u16::MAX` — "not numbered" —
                    // and every greyed track sorted to the bottom of its
                    // album instead of into the gap it belongs in.
                    (l.place_of(c.row.slot()), title, c.copy)
                });
            }
        }
    }
}

impl Tree {
    pub fn artists(&self) -> impl Iterator<Item = (&SortKey, &BTreeMap<SortKey, Album>)> {
        self.artists.iter()
    }

    /// What to show for an artist node. Falls back to the sort key, which is
    /// wrong-looking rather than absent — a node with no label cannot be
    /// clicked on.
    pub fn artist_name<'a>(&'a self, artist: &'a SortKey) -> &'a str {
        self.artist_names
            .get(artist)
            .map_or_else(|| artist.as_str(), String::as_str)
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — #59's reissue ordering, written with its reasoning and pinned by a test"
    )]
    /// An artist's albums, oldest first.
    ///
    /// **By the release group's date**, which is what #59 asked for: a 2015
    /// pressing of a 1982 album sorts as 1982, because the reissue's date is a
    /// fact about the pressing and never about the album.
    pub fn albums_by_date(&self, artist: &SortKey) -> Vec<&Album> {
        let mut albums: Vec<_> = self
            .artists
            .get(artist)
            .map(|a| a.values().collect())
            .unwrap_or_default();
        // An undated album sorts **last**, not first. `None` orders before
        // `Some` in Rust, which would file everything unknown at the top of an
        // artist and read as "these came first" — a claim the tags never made.
        // Same rule as an unnumbered track (`Librarian::place_of`): a gap in
        // what is known is not a position.
        albums.sort_by(|a, b| {
            match (a.first_released, b.first_released) {
                (Some(x), Some(y)) => x.cmp(&y),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
            // Belt and braces: the albums already arrive in key order from a
            // `BTreeMap`, so this only decides between two titles that fold to
            // the same key. No test covers it, because nothing can observe the
            // difference today — it is here so that a change of container
            // cannot silently make the order arbitrary.
            .then(a.title.cmp(&b.title))
        });
        albums
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::instance::{Instance, Scanned};

    /// Every row the library holds — what these tests group, said out loud
    /// now that the tree takes the set rather than assuming it (#121).
    fn everything(l: &Librarian) -> std::collections::HashSet<crate::model::rows::CopyId> {
        crate::model::rows::Listing::rows(l).into_iter().collect()
    }
    use crate::operations::populate::read_library;
    use crate::shelf::Level;

    /// The tree flattened — artist, then album, then disc and track.
    ///
    /// A helper rather than a method on `Tree`. `Shelf` does its own
    /// flattening for the rows it draws, and two ways to answer one question
    /// is the shape this project removes everywhere else (#113).
    fn flat(tree: &Tree) -> Vec<Listing> {
        tree.artists()
            .flat_map(|(_, albums)| albums.values().flat_map(|a| a.tracks.iter().copied()))
            .collect()
    }
    use crate::model::round_trip::{write_tags, write_wav};
    use std::path::PathBuf;

    fn scanned(title: &str, artist: &str, album_artist: Option<&str>, track: u16) -> Scanned {
        Scanned {
            audio: crate::audio_id::AudioId {
                scheme: 1,
                // Hash the title's bytes, not its length: `"a"` and `"z"`
                // are different songs, and a fixture that gives them the same
                // audio makes them the same recording.
                hash: title.bytes().fold(1u128, |h, b| h * 31 + b as u128) + track as u128,
            },
            title: title.into(),
            artist: artist.into(),
            album: Some("Big Science".into()),
            album_artist: album_artist.map(str::to_string),
            disc: Some(1),
            track: Some(track),
            ..Default::default()
        }
    }

    /// **The three shapes a row can be** — #155, worked end to end.
    ///
    /// One recording on two release groups, a recording on none, and a slot
    /// nothing fills. Each is drawn by a different arm of `CopyId`, and the
    /// point of the test is that all three coexist in one library.
    #[test]
    fn a_row_is_an_appearance_where_there_is_one_and_a_recording_where_there_is_not() {
        use crate::model::rows::CopyId;
        use crate::model::rows::Listing as Rows;

        // 1. One performance, two albums. Two files, because a rip from the
        //    compilation is not byte-identical to the one from the album.
        let mut on_album = scanned("Hells Bells", "AC/DC", Some("AC/DC"), 2);
        on_album.album = Some("Back in Black".into());
        on_album.identity = Some(crate::model::basis::Basis::MusicBrainz(
            uuid::Uuid::from_u128(0xbe11),
        ));
        let mut on_anthology = on_album.clone();
        on_anthology.album = Some("Who Made Who".into());
        on_anthology.audio.hash += 1;

        // 2. A recording on no release at all — a broadcast, an internet-only
        //    track, a YouTube rip. MusicBrainz calls this a standalone
        //    recording and does not require a release for it.
        let mut loose = scanned("A Rip From The Internet", "Somebody", None, 0);
        loose.album = None;

        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[on_album.clone(), on_anthology.clone(), loose.clone()],
        );

        // 3. A slot nothing fills: the library knows the track and holds no
        //    file for it. Written directly, because no pass writes one yet —
        //    that is the other half of #155.
        let cat = i.catalogue();
        let medium = *cat
            .media
            .iter()
            .find(|(_, m)| {
                cat.releases
                    .get(&m.release)
                    .and_then(|r| cat.groups.get(&r.group))
                    .is_some_and(|g| g.title == "Back in Black")
            })
            .expect("the album has a medium")
            .0;
        let unheld = Id::<crate::model::entity::Recording>::derived(b"a track we do not own");
        let slot = Id::<crate::model::entity::Track>::derived(b"slot 9 of back in black");
        let mut clock = crate::model::clock::HybridClock::new(Id::derived(b"laptop"));
        let unfilled: std::collections::BTreeSet<crate::facts::Record> = [
            crate::facts::Fact::RecordingCatalogued {
                recording: unheld,
                title: "Rock and Roll Ain't Noise Pollution".into(),
                artist: Id::derived(b"acdc"),
            },
            crate::facts::Fact::TrackPlaced {
                track: slot,
                medium,
                recording: unheld,
                position: 9,
                title_on_release: None,
            },
        ]
        .into_iter()
        .map(|fact| crate::facts::Record {
            stamp: clock.stamp(200),
            fact,
        })
        .collect();
        i.merge(200, &unfilled);

        let l = Librarian::from_catalogue(&i.catalogue());
        let rows = Rows::rows(&l);

        // **One recording, two appearances.** Two rows, one per album.
        let hells: Vec<CopyId> = rows
            .iter()
            .copied()
            .filter(|r| Rows::listed(&l, *r).is_some_and(|row| row.title == "Hells Bells"))
            .collect();
        assert_eq!(
            hells.len(),
            2,
            "a recording on two albums is two appearances"
        );
        let mut albums: Vec<String> = hells
            .iter()
            .filter_map(|r| Rows::listed(&l, *r).map(|row| row.album))
            .collect();
        albums.sort_unstable();
        assert_eq!(albums, vec!["Back in Black", "Who Made Who"]);
        assert!(
            hells.iter().all(|r| matches!(r, CopyId::On(_))),
            "both are appearances"
        );

        // **The standalone recording is a row and is not an appearance.**
        let stray = rows
            .iter()
            .copied()
            .find(|r| {
                Rows::listed(&l, *r).is_some_and(|row| row.title == "A Rip From The Internet")
            })
            .expect("a recording on no release is still a row");
        assert!(
            matches!(stray, CopyId::Loose(_)),
            "it belongs to no medium, and none was invented for it"
        );
        // **A file backs it**, which is the claim worth making here. Whether
        // *this device* can open it is `playable`, and no holding is published
        // in this fixture — `ingest` catalogues and `reconcile` publishes
        // (#145), so nothing here has a locator at all.
        assert!(
            l.shown_copy(stray).is_some(),
            "a standalone recording still has a copy behind it"
        );

        // **The slot nothing fills is a row, and it does not play.**
        let missing = rows
            .iter()
            .copied()
            .find(|r| {
                Rows::listed(&l, *r)
                    .is_some_and(|row| row.title == "Rock and Roll Ain't Noise Pollution")
            })
            .expect("a track we do not own is still a row");
        assert!(matches!(missing, CopyId::On(_)), "it is an appearance");
        assert_eq!(
            Rows::listed(&l, missing).map(|r| r.album),
            Some("Back in Black".to_owned()),
            "and it knows which album to be greyed under"
        );
        assert!(
            Rows::playable(&l, missing).is_none(),
            "nothing fills it, so there is nothing to open"
        );
        assert!(l.shown_copy(missing).is_none());
    }

    fn tree_of(scan: &[Scanned]) -> (Librarian, Tree) {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(100, scan);
        let l = Librarian::from_catalogue(&i.catalogue());
        let t = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );
        (l, t)
    }

    /// **Two tracks at the same number still come out the same way twice.**
    ///
    /// A position is not a total order and this library proves it: AC/DC's
    /// *High Voltage* is two different albums of that name — the 1975
    /// Australian and the 1976 international — sharing one folder, so six
    /// pairs of tracks carry the same number. Ordered by position alone, the
    /// two projections that existed before #116 disagreed about twelve rows,
    /// which is a library that looks different depending on which one drew it.
    ///
    /// Title first, then the copy's id: the id is the last resort and it is
    /// durable, so the tie breaks the same way after a restart too.
    #[test]
    fn two_tracks_at_one_number_are_ordered_by_title() {
        let (l, tree) = tree_of(&[
            scanned("Zebra", "AC/DC", None, 1),
            scanned("Antelope", "AC/DC", None, 1),
        ]);
        let titles: Vec<String> = tree
            .artists()
            .flat_map(|(_, albums)| albums.values())
            .flat_map(|a| a.tracks.iter())
            .filter_map(|c| l.row_of(c.recording, c.copy).map(|r| r.title))
            .collect();
        assert_eq!(
            titles,
            ["Antelope", "Zebra"],
            "two tracks at number one came out in listing order"
        );
    }

    /// **And two of the same name at the same number still have an order.**
    ///
    /// The last resort is the copy's id, which is derived from the bytes — so
    /// the tie breaks the same way after a restart and on another machine.
    /// Without it the order would be whatever the fold happened to yield,
    /// which is deterministic today by accident rather than by saying so.
    #[test]
    fn two_of_one_name_at_one_number_are_ordered_by_their_bytes() {
        let mut first = scanned("Little Lover", "AC/DC", None, 3);
        let mut second = scanned("Little Lover", "AC/DC", None, 3);
        first.audio.hash = 1;
        second.audio.hash = 2;

        let copy = |h: u128| {
            Id::<crate::model::entity::Encoding>::of_audio(&crate::audio_id::AudioId {
                scheme: 1,
                hash: h,
            })
        };

        let (_, tree) = tree_of(&[first, second]);
        let order: Vec<_> = tree
            .artists()
            .flat_map(|(_, albums)| albums.values())
            .flat_map(|a| a.tracks.iter())
            .filter_map(|c| c.copy)
            .collect();

        // **By the copy's id, and these two disagree with the recordings'.**
        //
        // Which is the whole point of naming it: derived from the same two
        // hashes, `of_audio` puts 1 before 2 and `of_basis` puts 2 before 1.
        // Drop the id from the key and a stable sort leaves them in the fold's
        // order — the recordings' — so this is what tells the two apart.
        assert_eq!(
            order,
            [copy(1), copy(2)],
            "the tie fell to whatever order the fold happened to yield"
        );
    }

    /// **A node has to be able to say its own name.**
    ///
    /// The tree is keyed by a sort key — case folded, marks flattened, and
    /// with the article moved when that is switched on — which is right for
    /// ordering and unreadable on screen. Without a label kept beside it every
    /// renderer has to reach back for a recording under the node just to
    /// recover the string.
    #[test]
    fn an_artist_node_shows_its_name_and_not_its_sort_key() {
        let (_, tree) = tree_of(&[scanned("Chest Fever", "The Band", Some("The Band"), 1)]);
        let (key, _) = tree.artists().next().expect("an artist");
        assert_eq!(key.as_str(), "the band", "the key orders");
        assert_eq!(tree.artist_name(key), "The Band", "the label reads");
    }

    /// **A reissue sorts as the album, not as the pressing** — #59, with the
    /// data it was waiting for.
    ///
    /// `first_released` was always `None`, so `albums_by_date` had nothing to
    /// order by and the trap it exists to avoid was unavoidable. The tags knew
    /// all along: `DATE` is the pressing and `ORIGINALDATE` the album, and
    /// they disagree for 820 files in this library — `Tri Repetae++` is a 1996
    /// pressing of a 1995 record.
    #[test]
    fn an_album_sorts_by_when_the_album_came_out() {
        let mut older = scanned("Second Bad Vilbel", "Autechre", Some("Autechre"), 1);
        older.album = Some("Tri Repetae++".into());
        older.released = Some(PartialDate::year(1996));
        older.first_released = Some(PartialDate::year(1995));

        let mut newer = scanned("Gantz Graf", "Autechre", Some("Autechre"), 1);
        newer.album = Some("Gantz Graf".into());
        newer.released = Some(PartialDate::year(2002));
        newer.first_released = Some(PartialDate::year(2002));

        let (_, tree) = tree_of(&[older, newer]);
        let (artist, _) = tree.artists().next().expect("an artist");
        let order: Vec<&str> = tree
            .albums_by_date(artist)
            .iter()
            .map(|a| a.title.as_str())
            .collect();
        assert_eq!(order, vec!["Tri Repetae++", "Gantz Graf"]);
        assert_eq!(
            tree.albums_by_date(artist)[0].first_released,
            Some(PartialDate::year(1995)),
            "the pressing's date was used, which is the trap itself"
        );
    }

    /// **A recording held by two albums: each copy files under its own.**
    ///
    /// One song, two copies on disk — the original album and a compilation —
    /// which the model correctly reads as one recording. Asked of the
    /// recording, `files_under` and `album_of` took
    /// `tracks_of_recording(..).first()`, so the copy sitting in the original
    /// album's folder was filed under the compilation. On the real library
    /// that moved tracks between 60 albums (#84).
    ///
    /// Asked of the **copy**, each has one slot and one answer. That is what
    /// #78 settles: a displayed row names a file, not a performance.
    #[test]
    fn each_copy_of_a_recording_files_under_its_own_album() {
        let mut original = scanned("Hells Bells", "AC/DC", Some("AC/DC"), 2);
        original.album = Some("Back in Black".into());
        original.identity = Some(crate::model::basis::Basis::MusicBrainz(
            uuid::Uuid::from_u128(0xbe11),
        ));
        let mut compilation = original.clone();
        compilation.album = Some("Who Made Who".into());
        // **Two files, so two encodings.** A rip from the compilation is not
        // byte-identical to the one from the album; giving them one hash would
        // make them one copy and test nothing.
        compilation.audio.hash += 1;
        // And both name the same performance, which is the whole premise.
        compilation.identity = original.identity.clone();

        let (l, tree) = tree_of(&[original.clone(), compilation.clone()]);

        // The tree lists both, each under its own album.
        let albums: Vec<&str> = tree
            .artists()
            .flat_map(|(_, a)| a.values().map(|al| al.title.as_str()))
            .collect();
        assert_eq!(
            albums,
            vec!["Back in Black", "Who Made Who"],
            "the tree showed one album for a song held on two"
        );
        assert_eq!(flat(&tree).len(), 2, "two copies, two rows");

        // Each copy knows its own slot, so each answers with its own album.
        let mut seen: Vec<String> = Vec::new();
        for s in [&original, &compilation] {
            let encoding = Id::<crate::model::entity::Encoding>::of_audio(&s.audio);
            let recording = l
                .encodings
                .encodings
                .get(&encoding)
                .and_then(|e| e.recording)
                .expect("the copy holds a recording")
                .id();
            let row = l.row_of(recording, Some(encoding)).expect("a row");
            seen.push(row.album.clone().unwrap_or_default());
        }
        seen.sort();
        assert_eq!(
            seen,
            vec!["Back in Black".to_string(), "Who Made Who".to_string()],
            "a copy was filed under the other copy's album"
        );
    }

    /// **The guest-credit case.** `file_under`'s heuristic reads the album
    /// artist off *each track*, so a track missing that tag splits the album.
    /// Asking the album's own credit cannot.
    #[test]
    fn a_guest_credit_does_not_split_the_album() {
        let (_, tree) = tree_of(&[
            scanned("Big Science", "Laurie Anderson", Some("Laurie Anderson"), 1),
            // A guest track: different track artist, same album artist.
            scanned(
                "Sweaters",
                "Laurie Anderson feat. Peter Gabriel",
                Some("Laurie Anderson"),
                2,
            ),
        ]);

        assert_eq!(tree.artists().count(), 1, "one artist, not two");
        assert_eq!(tree.artists().map(|(_, a)| a.len()).sum::<usize>(), 1);
        assert_eq!(flat(&tree).len(), 2);
    }

    /// Tracks come back in the order the tags claim, not the order they were
    /// scanned in.
    #[test]
    fn tracks_are_ordered_by_what_the_tags_claim() {
        let mut scan = vec![
            scanned("Third", "Laurie Anderson", None, 3),
            scanned("First", "Laurie Anderson", None, 1),
            scanned("Second", "Laurie Anderson", None, 2),
        ];
        scan.reverse();
        let (l, tree) = tree_of(&scan);

        let titles: Vec<_> = flat(&tree)
            .iter()
            .filter_map(|c| l.row_of(c.recording, c.copy))
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, ["First", "Second", "Third"]);
    }

    /// **A track you do not own sorts into its gap, not to the bottom** — #155.
    ///
    /// Found on the real library by looking at it: `Daft Punk — Homework` is
    /// missing `Teachers`, track 9, and drew it after track 16. The sort key
    /// asked `slot_of(copy)` — which slot is this *file* in — and a row with
    /// no file answered `None`, which `place_of` reads as "not numbered" and
    /// deliberately sorts last. It is the same mistake `row_of_via` had, at
    /// the one site that was missed: the question is which slot is this *row*
    /// in, and a row knows.
    #[test]
    fn a_track_nothing_fills_sorts_by_its_own_number() {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[
                scanned("One", "Laurie Anderson", None, 1),
                scanned("Three", "Laurie Anderson", None, 3),
            ],
        );

        // Track two, which the library knows about and holds no file for.
        let cat = i.catalogue();
        let medium = *cat.media.keys().next().expect("the album has a disc");
        let unheld = Id::<crate::model::entity::Recording>::derived(b"two");
        let slot = Id::<crate::model::entity::Track>::derived(b"slot two");
        let mut clock = crate::model::clock::HybridClock::new(Id::derived(b"laptop"));
        let records: std::collections::BTreeSet<crate::facts::Record> = [
            crate::facts::Fact::RecordingCatalogued {
                recording: unheld,
                title: "Two".into(),
                artist: Id::of_artist(None, "Laurie Anderson"),
            },
            crate::facts::Fact::TrackPlaced {
                track: slot,
                medium,
                recording: unheld,
                position: 2,
                title_on_release: None,
            },
        ]
        .into_iter()
        .map(|fact| crate::facts::Record {
            stamp: clock.stamp(200),
            fact,
        })
        .collect();
        i.merge(200, &records);

        let l = Librarian::from_catalogue(&i.catalogue());
        let tree = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );
        let titles: Vec<String> = {
            use crate::model::rows::Listing as _;
            flat(&tree)
                .iter()
                .filter_map(|c| l.listed(c.row).map(|r| r.title.to_string()))
                .collect()
        };
        assert_eq!(
            titles,
            ["One", "Two", "Three"],
            "the gap is where it belongs"
        );
    }

    /// **The reissue trap (#59).** A 2015 pressing of a 1982 album files under
    /// the album and sorts as 1982, because the group holds the date and the
    /// pressing's own date is a fact about the pressing.
    #[test]
    fn albums_sort_by_when_the_album_appeared() {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[
                scanned("Big Science", "Laurie Anderson", None, 1),
                Scanned {
                    album: Some("Mister Heartbreak".into()),
                    ..scanned("Sharkey's Day", "Laurie Anderson", None, 1)
                },
            ],
        );
        let mut l = Librarian::from_catalogue(&i.catalogue());

        // Dated as the albums are, not as any pressing was.
        for (id, g) in l.spine.groups.iter_mut() {
            let _ = id;
            g.first_released = Some(PartialDate::year(if g.title == "Big Science" {
                1982
            } else {
                1984
            }));
        }

        let tree = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );
        let artist = sort_key("Laurie Anderson", false);
        let by_date: Vec<_> = tree
            .albums_by_date(&artist)
            .iter()
            .map(|a| (a.title.clone(), a.first_released))
            .collect();

        assert_eq!(
            by_date,
            [
                ("Big Science".to_string(), Some(PartialDate::year(1982))),
                (
                    "Mister Heartbreak".to_string(),
                    Some(PartialDate::year(1984))
                ),
            ],
            "oldest album first, whatever order the tree happens to hold them in"
        );
    }

    /// Two albums with no date at all still order predictably, or the tree
    /// reshuffles between runs for no reason a reader could see.
    #[test]
    fn undated_albums_fall_back_to_their_titles() {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[
                Scanned {
                    album: Some("Zebra".into()),
                    ..scanned("z", "Laurie Anderson", None, 1)
                },
                Scanned {
                    album: Some("Aardvark".into()),
                    ..scanned("a", "Laurie Anderson", None, 1)
                },
            ],
        );
        let l = Librarian::from_catalogue(&i.catalogue());
        let tree = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );

        let titles: Vec<_> = tree
            .albums_by_date(&sort_key("Laurie Anderson", false))
            .iter()
            .map(|a| a.title.clone())
            .collect();
        assert_eq!(titles, ["Aardvark", "Zebra"]);
    }

    /// **Four folders by default, one shelf on request** — #174.
    ///
    /// Richard D. James releases as Aphex Twin and as AFX, and both are true
    /// at once: *Analogue Bubblebath* belongs under AFX exactly as the sleeve
    /// says, and "everything he made" is still a thing to ask. The default
    /// level keeps them apart; `@1` puts them together, and Adele — who has
    /// no relation and needs none — stays exactly where she was.
    #[test]
    fn a_moniker_files_apart_by_default_and_together_at_a_level() {
        use crate::model::reference::ArtistRelationKind;

        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[
                Scanned {
                    album: Some("Analogue Bubblebath".into()),
                    ..scanned("ab", "AFX", None, 1)
                },
                Scanned {
                    album: Some("Selected Ambient Works".into()),
                    ..scanned("saw", "Aphex Twin", None, 1)
                },
                Scanned {
                    album: Some("21".into()),
                    ..scanned("21", "Adele", None, 1)
                },
            ],
        );
        let cat = i.catalogue();
        let named = |who: &str| {
            *cat.artist_names
                .iter()
                .find(|(_, n)| n.as_str() == who)
                .expect("ingested")
                .0
        };
        i.relate_artists(
            101,
            named("AFX"),
            named("Aphex Twin"),
            ArtistRelationKind::AliasOf,
        );
        let l = Librarian::from_catalogue(&i.catalogue());

        let shelves = |at: Option<u8>| {
            let tree = Tree::grouped(
                &l,
                (
                    crate::shelf::Level {
                        field: Field::FileUnder,
                        at,
                    },
                    Level::of(Field::Album),
                ),
                false,
                &everything(&l),
            );
            let mut names: Vec<String> = tree.artist_names.values().cloned().collect();
            names.sort();
            names
        };

        assert_eq!(
            shelves(None),
            ["AFX", "Adele", "Aphex Twin"],
            "the sleeve's own credit is the default, and it should stay one"
        );
        assert_eq!(
            shelves(Some(1)),
            ["Adele", "Aphex Twin"],
            "one shelf for the act, and Adele untouched by a graph she is not in"
        );
    }

    /// **Dated before undated, and the date is what decides.** An ordering
    /// that agreed with the titles by accident would say nothing.
    #[test]
    fn a_dated_album_sorts_by_its_date_not_its_name() {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(
            100,
            &[
                Scanned {
                    album: Some("Aardvark".into()),
                    ..scanned("a", "Laurie Anderson", None, 1)
                },
                Scanned {
                    album: Some("Zebra".into()),
                    ..scanned("z", "Laurie Anderson", None, 1)
                },
            ],
        );
        let mut l = Librarian::from_catalogue(&i.catalogue());
        // Zebra is older, and sorts first *against* the alphabet.
        for g in l.spine.groups.values_mut() {
            if g.title == "Zebra" {
                g.first_released = Some(PartialDate::year(1970));
            }
        }
        let tree = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );
        let titles: Vec<_> = tree
            .albums_by_date(&sort_key("Laurie Anderson", false))
            .iter()
            .map(|a| a.title.clone())
            .collect();
        assert_eq!(
            titles,
            ["Zebra", "Aardvark"],
            "the date decides, and it disagrees with the alphabet here"
        );
    }

    /// **A track with no number sorts last, not first.** Zero is a position;
    /// "no idea" is not, and putting an unnumbered track at the top of an
    /// album states something the tags never did.
    #[test]
    fn an_unnumbered_track_does_not_jump_to_the_front() {
        let (l, tree) = tree_of(&[
            scanned("First", "Laurie Anderson", None, 1),
            Scanned {
                track: None,
                ..scanned("Nowhere", "Laurie Anderson", None, 9)
            },
            scanned("Second", "Laurie Anderson", None, 2),
        ]);
        let titles: Vec<_> = flat(&tree)
            .iter()
            .filter_map(|c| l.row_of(c.recording, c.copy))
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, ["First", "Second", "Nowhere"]);
    }

    /// A recording on no album is an ordinary row under a named heading, not a
    /// hole.
    #[test]
    fn something_with_no_album_still_files_somewhere() {
        let (_, tree) = tree_of(&[Scanned {
            album: None,
            ..scanned("Loose", "Laurie Anderson", None, 1)
        }]);
        let artist = sort_key("Laurie Anderson", false);
        let albums = tree.albums_by_date(&artist);
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].title, NO_ALBUM);
        assert_eq!(albums[0].group, None);
    }

    /// Over real files, end to end.
    #[test]
    fn a_disk_becomes_a_tree() {
        let dir = std::env::temp_dir().join("gmuse-tree-disk");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (n, title) in ["Racing", "Straightjacket"].iter().enumerate() {
            let p = dir.join(format!("{:02} {title}.wav", n + 1));
            write_wav(&p, 4410 + n * 11);
            write_tags(&p, title, "Early James", None);
        }

        let (instance, report) = read_library("laptop", &[PathBuf::from(&dir)], 100);
        assert_eq!(report.ingested, 2);
        let l = Librarian::from_catalogue(&instance.catalogue());
        let tree = Tree::grouped(
            &l,
            (Level::of(Field::FileUnder), Level::of(Field::Album)),
            false,
            &everything(&l),
        );

        assert_eq!(tree.artists().count(), 1);
        assert_eq!(flat(&tree).len(), 2);
    }
}
