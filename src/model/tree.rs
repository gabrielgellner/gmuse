//! The artist → album → track tree, over the model — issue #84.
//!
//! `library.rs` builds this shape from a `Store`, keyed by an arena index and
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

use std::collections::BTreeMap;

use crate::model::entity::{Recording, ReleaseGroup};
use crate::model::id::Id;
use crate::model::seam::Librarian;
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
    pub tracks: Vec<Id<Recording>>,
}

/// The tree a browser walks.
#[derive(Debug, Default)]
pub struct Tree {
    artists: BTreeMap<SortKey, BTreeMap<SortKey, Album>>,
    flat: Vec<Id<Recording>>,
}

/// Shown for a recording that belongs to no album.
pub const NO_ALBUM: &str = "No Album";

impl Tree {
    /// Build the tree a library describes.
    pub fn of(l: &Librarian, smart_artist_sort: bool) -> Self {
        let mut by_artist: BTreeMap<SortKey, BTreeMap<SortKey, Album>> = BTreeMap::new();

        for recording in l.recordings() {
            let Some(row) = l.row(recording) else {
                continue;
            };
            let artist = sort_key(&row.file_under, smart_artist_sort);
            // Albums keep their article: "The Wall" belongs under T.
            let title = row.album.clone().unwrap_or_else(|| NO_ALBUM.to_string());
            let album_key = sort_key(&title, false);
            let group = l.group_of(recording);

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
                .push(recording);
        }

        for albums in by_artist.values_mut() {
            for album in albums.values_mut() {
                album.tracks.sort_by_key(|r| l.place_of(*r));
            }
        }
        let flat = by_artist
            .values()
            .flat_map(|albums| albums.values().flat_map(|a| a.tracks.iter().copied()))
            .collect();

        Tree {
            artists: by_artist,
            flat,
        }
    }

    pub fn artists(&self) -> impl Iterator<Item = (&SortKey, &BTreeMap<SortKey, Album>)> {
        self.artists.iter()
    }

    /// The whole tree flattened — artist, then album, then disc and track.
    pub fn flat(&self) -> &[Id<Recording>] {
        &self.flat
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
                hash: title.len() as u128 * 7919 + track as u128,
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
            .filter_map(|r| l.row(*r))
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
            .filter_map(|r| l.row(*r))
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
