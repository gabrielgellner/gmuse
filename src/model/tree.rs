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
    flat: Vec<Listing>,
}

/// Shown for a recording that belongs to no album.
pub const NO_ALBUM: &str = "No Album";

impl Tree {
    /// Build the tree a library describes.
    pub fn of(l: &Librarian, smart_artist_sort: bool) -> Self {
        let mut by_artist: BTreeMap<SortKey, BTreeMap<SortKey, Album>> = BTreeMap::new();
        let mut artist_names: BTreeMap<SortKey, String> = BTreeMap::new();

        // **One node per copy, not per recording** — issue #78. A recording is
        // on every release that holds it; the file you have is on one, and the
        // tree is a list of what you have.
        for recording in l.recordings() {
            for listing in l.listings(recording) {
                let Some(row) = l.row_of(recording, listing.copy) else {
                    continue;
                };
                let slot = l.slot_of(listing.copy);
                let artist = sort_key(&row.file_under, smart_artist_sort);
                artist_names
                    .entry(artist.clone())
                    .or_insert_with(|| row.file_under.clone());
                // Albums keep their article: "The Wall" belongs under T.
                let title = row.album.clone().unwrap_or_else(|| NO_ALBUM.to_string());
                let album_key = sort_key(&title, false);
                let group = l.group_of(slot);

                by_artist
                    .entry(artist)
                    .or_default()
                    .entry(album_key)
                    .or_insert_with(|| Album {
                        group,
                        title,
                        first_released: group
                            .and_then(|g| l.spine.groups.get(&g))
                            .and_then(|g| g.first_released),
                        tracks: Vec::new(),
                    })
                    .tracks
                    .push(listing);
            }
        }

        for albums in by_artist.values_mut() {
            for album in albums.values_mut() {
                album.tracks.sort_by_key(|c| l.place_of(l.slot_of(c.copy)));
            }
        }
        let flat = by_artist
            .values()
            .flat_map(|albums| albums.values().flat_map(|a| a.tracks.iter().copied()))
            .collect();

        Tree {
            artists: by_artist,
            artist_names,
            flat,
        }
    }

    pub fn artists(&self) -> impl Iterator<Item = (&SortKey, &BTreeMap<SortKey, Album>)> {
        self.artists.iter()
    }

    /// The whole tree flattened — artist, then album, then disc and track.
    pub fn flat(&self) -> &[Listing] {
        &self.flat
    }

    /// What to show for an artist node. Falls back to the sort key, which is
    /// wrong-looking rather than absent — a node with no label cannot be
    /// clicked on.
    pub fn artist_name<'a>(&'a self, artist: &'a SortKey) -> &'a str {
        self.artist_names
            .get(artist)
            .map_or_else(|| artist.as_str(), String::as_str)
    }

    pub fn artist_count(&self) -> usize {
        self.artists.len()
    }

    pub fn album_count(&self) -> usize {
        self.artists.values().map(BTreeMap::len).sum()
    }

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
    use crate::model::populate::read_library;
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

    fn tree_of(scan: &[Scanned]) -> (Librarian, Tree) {
        let mut i = Instance::new(Id::derived(b"laptop"));
        i.ingest(100, scan);
        let l = Librarian::from_catalogue(&i.catalogue());
        let t = Tree::of(&l, false);
        (l, t)
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
        assert_eq!(tree.flat().len(), 2, "two copies, two rows");

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

        assert_eq!(tree.artist_count(), 1, "one artist, not two");
        assert_eq!(tree.album_count(), 1);
        assert_eq!(tree.flat().len(), 2);
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

        let titles: Vec<_> = tree
            .flat()
            .iter()
            .filter_map(|c| l.row_of(c.recording, c.copy))
            .map(|r| r.title)
            .collect();
        assert_eq!(titles, ["First", "Second", "Third"]);
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

        let tree = Tree::of(&l, false);
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
        let tree = Tree::of(&l, false);

        let titles: Vec<_> = tree
            .albums_by_date(&sort_key("Laurie Anderson", false))
            .iter()
            .map(|a| a.title.clone())
            .collect();
        assert_eq!(titles, ["Aardvark", "Zebra"]);
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
        let tree = Tree::of(&l, false);
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
        let titles: Vec<_> = tree
            .flat()
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
        let tree = Tree::of(&l, false);

        assert_eq!(tree.artist_count(), 1);
        assert_eq!(tree.flat().len(), 2);
    }
}
