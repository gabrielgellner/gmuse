//! Where what you hold is arranged, so you can find it — DESIGN §4.
//!
//! It was called `Shelf` after this rename and `Library` before it, and the
//! old name claimed too much in the same way `Store` did: this holds no
//! truth, only an arrangement of it. Views 1 and 2 are one membership in two
//! shapes — a tree by artist then album, and the same tracks in one order.
//!
//! Named for the vocabulary the rest of the model speaks — catalogue, spine,
//! holdings, imprint — and deliberately not `view`, which #92 is about to
//! give a precise meaning over the model rather than over `TrackId`s.
//!
//! Views 1 and 2 share membership and differ only in shape: view 1 is an
//! artist → album → tracks tree, view 2 the same tracks as one sorted list.
//! Both are rebuilt from the `Scan`, never mutated in place, which is what
//! makes `TrackId` indices worth having.

use std::collections::{BTreeMap, HashSet};

use crate::model::entity::Encoding;
use crate::model::id::Id;
use crate::model::seam::Librarian;
use crate::store::{Scan, Track, TrackId};

/// Re-exported so callers keep saying `shelf::SortKey`. It lives in the
/// model now, because how a library orders its own names is a librarian
/// question rather than a player one (#84).
pub use crate::model::sortkey::{SortKey, sort_key};

/// Views 1 and 2. Same membership, two shapes.
#[derive(Debug, Default)]
pub struct Shelf {
    members: HashSet<TrackId>,
    tree: BTreeMap<SortKey, BTreeMap<SortKey, Vec<TrackId>>>,
    /// What to *show* for each sort key.
    ///
    /// The tree is keyed by a normalised sort key — case folded, marks
    /// flattened, article moved — which is right for ordering and unreadable
    /// on screen. Without this every renderer has to reach back into the
    /// `Scan` for a track under the node just to recover the string, which is
    /// how a projection that exists to answer questions ends up unable to
    /// answer the simplest one.
    artist_names: BTreeMap<SortKey, String>,
    album_names: BTreeMap<(SortKey, SortKey), String>,
    sorted: Vec<TrackId>,
    smart_artist_sort: bool,
}

// Some of this is ahead of its callers: the filter and command layers (M6,
// M8) are what will ask a library whether it contains a track.
#[allow(dead_code)]
impl Shelf {
    /// Rebuild both projections from every track in the store.
    pub fn rebuild(store: &Scan, smart_artist_sort: bool) -> Self {
        Self::from_members(
            store,
            store.iter().map(|(id, _)| id).collect(),
            smart_artist_sort,
        )
    }

    /// Rebuild from an explicit membership.
    ///
    /// This is how catalogs work: §4 makes the library a projection over a
    /// *subset* of the store, so confining the subset confines browsing,
    /// shuffle and `aaa_mode` without `next()` knowing catalogs exist.
    pub fn from_members(store: &Scan, members: HashSet<TrackId>, smart_artist_sort: bool) -> Self {
        let mut lib = Shelf {
            members,
            smart_artist_sort,
            ..Default::default()
        };
        lib.reindex(store);
        lib
    }

    /// Rebuild from what the **model** says — issue #84.
    ///
    /// The shape and every caller are unchanged: this still answers in
    /// `TrackId`, because the player holds handles and dissolving that is a
    /// separate move. What changes is where the two grouping decisions come
    /// from.
    ///
    /// **Artist grouping asks for a credit** rather than picking album artist
    /// else artist, which guesses at something the model states — and guesses
    /// wrong on exactly the track carrying a guest credit.
    ///
    /// **The album level is the release group of the copy's own slot**, so two
    /// pressings stop being two albums, and a song held on both an album and a
    /// compilation appears under each rather than under whichever placement
    /// sorted first (#78).
    ///
    /// A file the model has no listing for keeps its tag-derived place rather
    /// than vanishing. Not tidiness: a library is browsed while it is being
    /// catalogued, and a file dropping out of the tree because nothing has
    /// identified it yet would be the worst possible moment to lose it.
    pub fn project(
        librarian: &Librarian,
        store: &Scan,
        members: HashSet<TrackId>,
        smart_artist_sort: bool,
    ) -> Self {
        let mut lib = Shelf {
            members,
            smart_artist_sort,
            ..Default::default()
        };

        // Which files hold each copy. A copy is bytes, so two files with
        // identical audio are one copy held twice — both belong in the tree.
        let mut files: BTreeMap<Id<Encoding>, Vec<TrackId>> = BTreeMap::new();
        for &id in &lib.members {
            if let Some(audio) = store.get(id).and_then(|t| t.audio_id) {
                files.entry(Id::of_audio(&audio)).or_default().push(id);
            }
        }

        let tree = crate::model::tree::Tree::of(librarian, smart_artist_sort);
        let mut placed: HashSet<TrackId> = HashSet::new();
        for (artist_key, albums) in tree.artists() {
            for (album_key, album) in albums {
                for listing in &album.tracks {
                    let Some(here) = listing.copy.and_then(|c| files.get(&c)) else {
                        continue;
                    };
                    lib.artist_names
                        .entry(artist_key.clone())
                        .or_insert_with(|| tree.artist_name(artist_key).to_string());
                    lib.album_names
                        .entry((artist_key.clone(), album_key.clone()))
                        .or_insert_with(|| album.title.clone());
                    let into = lib
                        .tree
                        .entry(artist_key.clone())
                        .or_default()
                        .entry(album_key.clone())
                        .or_default();
                    for &track in here {
                        into.push(track);
                        placed.insert(track);
                    }
                }
            }
        }

        let unplaced: HashSet<TrackId> = lib.members.difference(&placed).copied().collect();
        lib.file_by_tag(store, &unplaced);
        lib.sorted = lib
            .tree
            .values()
            .flat_map(|albums| albums.values().flatten().copied())
            .collect();
        lib
    }

    /// File tracks under the names their tags carry — the pre-#84 rule, kept
    /// for whatever the model cannot place.
    fn file_by_tag(&mut self, store: &Scan, ids: &HashSet<TrackId>) {
        for &id in ids {
            let Some(track) = store.get(id) else { continue };
            let artist = sort_key(track.file_under(), self.smart_artist_sort);
            let album = sort_key(track.album_or_unknown(), false);
            self.artist_names
                .entry(artist.clone())
                .or_insert_with(|| track.file_under().to_string());
            self.album_names
                .entry((artist.clone(), album.clone()))
                .or_insert_with(|| track.album_or_unknown().to_string());
            self.tree
                .entry(artist)
                .or_default()
                .entry(album)
                .or_default()
                .push(id);
        }
        for albums in self.tree.values_mut() {
            for tracks in albums.values_mut() {
                tracks.sort_by(|a, b| track_order(store, *a, *b));
            }
        }
    }

    fn reindex(&mut self, store: &Scan) {
        self.tree.clear();
        self.sorted.clear();
        self.artist_names.clear();
        self.album_names.clear();

        for &id in &self.members {
            let Some(track) = store.get(id) else { continue };
            let artist = sort_key(track.file_under(), self.smart_artist_sort);
            // Albums keep their article: "The Wall" belongs under T.
            let album = sort_key(track.album_or_unknown(), false);
            // First one under a node names it, which is what every renderer
            // was doing by hand.
            self.artist_names
                .entry(artist.clone())
                .or_insert_with(|| track.file_under().to_string());
            self.album_names
                .entry((artist.clone(), album.clone()))
                .or_insert_with(|| track.album_or_unknown().to_string());
            self.tree
                .entry(artist)
                .or_default()
                .entry(album)
                .or_default()
                .push(id);
            self.sorted.push(id);
        }

        for albums in self.tree.values_mut() {
            for tracks in albums.values_mut() {
                tracks.sort_by(|a, b| track_order(store, *a, *b));
            }
        }
        // View 2 is the tree flattened: artist, then album, then disc/track.
        self.sorted = self
            .tree
            .values()
            .flat_map(|albums| albums.values().flatten().copied())
            .collect();
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// View 2: every track, in tree order.
    pub fn sorted(&self) -> &[TrackId] {
        &self.sorted
    }

    /// Artists, in sort order.
    /// What to show for an artist, rather than what to sort it by.
    pub fn artist_name<'a>(&'a self, artist: &'a SortKey) -> &'a str {
        self.artist_names
            .get(artist)
            .map(String::as_str)
            .unwrap_or_else(|| artist.as_str())
    }

    /// What to show for an album under an artist.
    pub fn album_name<'a>(&'a self, artist: &SortKey, album: &'a SortKey) -> &'a str {
        self.album_names
            .get(&(artist.clone(), album.clone()))
            .map(String::as_str)
            .unwrap_or_else(|| album.as_str())
    }

    pub fn artists(&self) -> impl Iterator<Item = (&SortKey, &BTreeMap<SortKey, Vec<TrackId>>)> {
        self.tree.iter()
    }

    pub fn artist_count(&self) -> usize {
        self.tree.len()
    }

    pub fn album_count(&self) -> usize {
        self.tree.values().map(|a| a.len()).sum()
    }

    pub fn contains(&self, id: TrackId) -> bool {
        self.members.contains(&id)
    }

    /// Which artist and album a track sits under.
    ///
    /// `aaa_mode` needs this to narrow the candidate pool, and getting it from
    /// the tree rather than the `Scan` keeps `next()`'s inputs to exactly what
    /// DESIGN §5 lists.
    pub fn locate(&self, id: TrackId) -> Option<(&SortKey, &SortKey)> {
        self.tree.iter().find_map(|(artist, albums)| {
            albums
                .iter()
                .find(|(_, tracks)| tracks.contains(&id))
                .map(|(album, _)| (artist, album))
        })
    }

    /// Every track by one artist, in album then disc/track order.
    pub fn artist_tracks(&self, artist: &SortKey) -> Vec<TrackId> {
        self.tree
            .get(artist)
            .map(|albums| albums.values().flatten().copied().collect())
            .unwrap_or_default()
    }

    /// One album's tracks, in disc/track order.
    pub fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> &[TrackId] {
        self.tree
            .get(artist)
            .and_then(|albums| albums.get(album))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

/// Ordering of tracks inside one album: disc, then track number, then title.
///
/// Disc first is what makes a multi-disc album read as 1-01, 1-02, … 2-01
/// rather than interleaving the two discs' track 1s. A missing disc number
/// sorts as disc 1, since single-disc albums usually omit it entirely.
fn track_order(store: &Scan, a: TrackId, b: TrackId) -> std::cmp::Ordering {
    let (Some(ta), Some(tb)) = (store.get(a), store.get(b)) else {
        return std::cmp::Ordering::Equal;
    };
    key_of(ta).cmp(&key_of(tb))
}

fn key_of(t: &Track) -> (u32, u32, String, &std::path::PathBuf) {
    (
        t.discnumber.unwrap_or(1),
        // Untracked tracks sort after numbered ones rather than before.
        t.tracknumber.unwrap_or(u32::MAX),
        t.display_title().to_lowercase(),
        &t.path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::time::Duration;

    fn t(path: &str, artist: Option<&str>, album: Option<&str>, title: Option<&str>) -> Track {
        crate::store::heard(Track {
            path: PathBuf::from(path),
            artist: artist.map(String::from),
            album: album.map(String::from),
            title: title.map(String::from),
            duration: Duration::from_secs(1),
            ..Default::default()
        })
    }

    #[test]
    fn smart_sort_moves_the_article_without_losing_it() {
        assert_eq!(sort_key("The Beatles", true).as_str(), "beatles");
        assert_eq!(
            sort_key("A Tribe Called Quest", true).as_str(),
            "tribe called quest"
        );
        assert_eq!(sort_key("An Horse", true).as_str(), "horse");
        // Off, the article stays.
        assert_eq!(sort_key("The Beatles", false).as_str(), "the beatles");
        // Not an article, just a word starting with the same letters.
        assert_eq!(sort_key("Theatre Royal", true).as_str(), "theatre royal");
        // A band actually called "The" must not vanish.
        assert_eq!(sort_key("The", true).as_str(), "the");
    }

    #[test]
    fn compilations_group_under_albumartist() {
        let mut store = Scan::new();
        for (path, artist, title) in [
            ("/va/01.mp3", "Benjamin Tod", "The Mountain"),
            ("/va/02.mp3", "Sabine McCalla", "Baby, Please Don't Go"),
            ("/va/03.mp3", "Sierra Ferrell", "Years"),
        ] {
            let mut track = t(path, Some(artist), Some("YouTube Sessions"), Some(title));
            track.albumartist = Some("Various Artists".into());
            store.upsert(track);
        }

        let lib = Shelf::rebuild(&store, true);
        // One artist, not three: the whole point of the albumartist rule.
        assert_eq!(lib.artist_count(), 1);
        assert_eq!(lib.album_count(), 1);
        let (artist, albums) = lib.artists().next().unwrap();
        assert_eq!(artist.as_str(), "various artists");
        assert_eq!(albums.values().next().unwrap().len(), 3);
    }

    #[test]
    fn multi_disc_albums_order_by_disc_then_track() {
        let mut store = Scan::new();
        // Deliberately inserted out of order.
        for (disc, num, title) in [
            (2u32, 1u32, "disc two, one"),
            (1, 2, "disc one, two"),
            (2, 2, "disc two, two"),
            (1, 1, "disc one, one"),
        ] {
            let mut track = t(
                &format!("/m/d{disc}t{num}.mp3"),
                Some("Artist"),
                Some("Album"),
                Some(title),
            );
            track.discnumber = Some(disc);
            track.tracknumber = Some(num);
            store.upsert(track);
        }

        let lib = Shelf::rebuild(&store, true);
        let titles: Vec<&str> = lib
            .sorted()
            .iter()
            .map(|id| store.get(*id).unwrap().display_title())
            .collect();
        assert_eq!(
            titles,
            [
                "disc one, one",
                "disc one, two",
                "disc two, one",
                "disc two, two"
            ],
            "discs must not interleave"
        );
    }

    #[test]
    fn missing_tags_land_under_unknown_and_keep_the_file_name() {
        let mut store = Scan::new();
        store.upsert(t("/m/mystery.mp3", None, None, None));
        store.upsert(t(
            "/m/known.mp3",
            Some("Burial"),
            Some("Untrue"),
            Some("Archangel"),
        ));

        let lib = Shelf::rebuild(&store, true);
        assert_eq!(lib.artist_count(), 2);

        let unknown = lib
            .artists()
            .find(|(k, _)| k.as_str() == "unknown artist")
            .expect("untagged track should still appear");
        let album = unknown.1.values().next().unwrap();
        let track = store.get(album[0]).unwrap();
        assert_eq!(track.display_title(), "mystery.mp3");
    }

    #[test]
    fn untracked_tracks_sort_after_numbered_ones() {
        let mut store = Scan::new();
        let mut a = t("/m/a.mp3", Some("X"), Some("Y"), Some("no number"));
        a.tracknumber = None;
        let mut b = t("/m/b.mp3", Some("X"), Some("Y"), Some("number 9"));
        b.tracknumber = Some(9);
        store.upsert(a);
        store.upsert(b);

        let lib = Shelf::rebuild(&store, true);
        let titles: Vec<&str> = lib
            .sorted()
            .iter()
            .map(|id| store.get(*id).unwrap().display_title())
            .collect();
        assert_eq!(titles, ["number 9", "no number"]);
    }

    #[test]
    fn view_two_is_the_tree_flattened() {
        let mut store = Scan::new();
        store.upsert(t("/m/1.mp3", Some("Zebra"), Some("Album"), Some("z")));
        store.upsert(t(
            "/m/2.mp3",
            Some("The Aardvarks"),
            Some("Album"),
            Some("a"),
        ));

        let lib = Shelf::rebuild(&store, true);
        let artists: Vec<&str> = lib.artists().map(|(k, _)| k.as_str()).collect();
        assert_eq!(artists, ["aardvarks", "zebra"]);
        // Same order in the flat view.
        let first = store.get(lib.sorted()[0]).unwrap();
        assert_eq!(first.artist.as_deref(), Some("The Aardvarks"));
        assert_eq!(lib.sorted().len(), 2);
    }

    /// `ö` is U+00F6 and `z` is U+007A, so an unflattened key files Löffler
    /// past every artist beginning `lz` — off the end of the L section.
    #[test]
    fn an_umlaut_sorts_among_its_own_letters_not_after_z() {
        let mut keys = [
            sort_key("Lz Top", false),
            sort_key("Christian Löffler", false),
            sort_key("Lonnie Holley", false),
            sort_key("La Luz", false),
        ];
        keys.sort();
        let order: Vec<&str> = keys.iter().map(SortKey::as_str).collect();
        assert_eq!(
            order,
            ["christian loffler", "la luz", "lonnie holley", "lz top"],
            "Löffler must file under Lo, not after Lz"
        );
    }

    #[test]
    fn a_leading_article_is_still_stripped_after_flattening() {
        assert_eq!(
            sort_key("The Österreichers", true).as_str(),
            "osterreichers"
        );
    }
}

#[cfg(test)]
mod projection {
    //! **Where the two grouping decisions come from** — issue #84.

    use super::*;
    use crate::model::instance::{Instance, Scanned};

    fn scan(title: &str, artist: &str, album_artist: Option<&str>, album: &str) -> Scanned {
        Scanned {
            audio: crate::audio_id::AudioId {
                scheme: 1,
                hash: title.bytes().fold(1u128, |h, b| h * 31 + b as u128)
                    + album.len() as u128 * 7919,
            },
            title: title.into(),
            artist: artist.into(),
            album: Some(album.into()),
            album_artist: album_artist.map(str::to_string),
            disc: Some(1),
            track: Some(1),
            ..Default::default()
        }
    }

    fn build(scan: &[Scanned]) -> (Scan, Shelf) {
        let mut i = Instance::new(crate::model::id::Id::derived(b"laptop"));
        i.ingest(100, scan);
        let l = Librarian::from_catalogue(&i.catalogue());

        let mut store = Scan::default();
        for s in scan {
            let mut t = Track {
                path: std::path::PathBuf::from(format!(
                    "/m/{}-{}.mp3",
                    s.album.as_deref().unwrap_or(""),
                    s.title
                )),
                ..Default::default()
            };
            t.artist = Some(s.artist.clone());
            t.albumartist = s.album_artist.clone();
            t.album = s.album.clone();
            t.title = Some(s.title.clone());
            t.audio_id = Some(s.audio);
            store.upsert(t);
        }
        let members: HashSet<TrackId> = store.iter().map(|(i, _)| i).collect();
        let lib = Shelf::project(&l, &store, members, false);
        (store, lib)
    }

    /// **Two pressings of one album are one album**, which the tags cannot
    /// say.
    ///
    /// `Track::album_or_unknown` reads the album string off each file, so a
    /// reissue whose tag capitalises differently is a second album node. The
    /// model groups by the release group both pressings name, and the group
    /// has one title.
    #[test]
    fn the_album_comes_from_the_release_group() {
        let mut a = scan("Callisto", "Bodzin", Some("Bodzin"), "Luna");
        let mut b = scan("Oberon", "Bodzin", Some("Bodzin"), "Luna (Deluxe Edition)");
        // One album to MusicBrainz; two pressings, and the tags say so.
        let group = uuid::Uuid::from_u128(0x9e11);
        a.mb_release_group = Some(group);
        b.mb_release_group = Some(group);
        let (store, lib) = build(&[a.clone(), b.clone()]);

        assert_eq!(lib.album_count(), 1, "two pressings, one album");

        let members: HashSet<TrackId> = store.iter().map(|(i, _)| i).collect();
        assert_eq!(
            Shelf::from_members(&store, members, false).album_count(),
            2,
            "the tag reading has to disagree, or this test proves nothing"
        );
    }

    /// **A file the model cannot place is still in the tree.** A library is
    /// browsed while it is being catalogued.
    #[test]
    fn something_the_model_has_no_listing_for_is_kept() {
        let mut store = Scan::default();
        let mut t = crate::store::heard(Track {
            path: std::path::PathBuf::from("/m/x.mp3"),
            ..Default::default()
        });
        t.artist = Some("Nobody".into());
        t.album = Some("Untagged".into());
        store.upsert(t);
        let members: HashSet<TrackId> = store.iter().map(|(i, _)| i).collect();

        let lib = Shelf::project(&Librarian::new(), &store, members, false);
        assert_eq!(lib.sorted().len(), 1, "an unidentified file vanished");
        assert_eq!(lib.artist_count(), 1);
    }
}
