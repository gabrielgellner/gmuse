//! Where what you hold is arranged, so you can find it — DESIGN §4.
//!
//! It was called `Shelf` after this rename and `Library` before it, and the
//! old name claimed too much in the same way `Store` did: this holds no
//! truth, only an arrangement of it. Views 1 and 2 are one membership in two
//! shapes — a tree by artist then album, and the same tracks in one order.
//!
//! Named for the vocabulary the rest of the model speaks — catalogue, spine,
//! holdings, imprint — and deliberately not `view`, which #92 is about to
//! give a precise meaning over the model rather than over `CopyId`s.
//!
//! Views 1 and 2 share membership and differ only in shape: view 1 is an
//! artist → album → tracks tree, view 2 the same tracks as one sorted list.
//! Both are rebuilt from the `Scan`, never mutated in place, which is what
//! makes `CopyId` indices worth having.

use std::collections::{BTreeMap, HashSet};

use crate::model::rows::CopyId;
use crate::model::rows::Listing;
use crate::model::seam::Librarian;

/// Re-exported so callers keep saying `shelf::SortKey`. It lives in the
/// model now, because how a library orders its own names is a librarian
/// question rather than a player one (#84).
pub use crate::model::sortkey::{SortKey, sort_key};

/// Views 1 and 2. Same membership, two shapes.
#[derive(Debug, Default)]
pub struct Shelf {
    members: HashSet<CopyId>,
    tree: BTreeMap<SortKey, BTreeMap<SortKey, Vec<CopyId>>>,
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
    sorted: Vec<CopyId>,
}

// **`smart_artist_sort` was a field here and is not one any more** (#116). It
// was held so `reindex` could fold a name the same way twice; there is one
// projection now, and it passes the setting straight to `Tree::of`, which is
// the only thing that folds a name at all.

// Some of this is ahead of its callers: the filter and command layers (M6,
// M8) are what will ask a library whether it contains a track.
#[allow(dead_code)]
impl Shelf {
    /// Every row the library holds.
    pub fn rebuild(librarian: &Librarian, smart_artist_sort: bool) -> Self {
        let all = Listing::rows(librarian).into_iter().collect();
        Self::from_members(librarian, all, smart_artist_sort)
    }

    /// Rebuild from an explicit membership.
    ///
    /// This is how catalogs work: §4 makes the library a projection over a
    /// *subset* of the rows, so confining the subset confines browsing,
    /// shuffle and `aaa_mode` without `next()` knowing catalogs exist.
    ///
    /// **There was a second one of these** (#116). `project` grouped by what
    /// the model says and this grouped by what the tags carried, and #84 chose
    /// between them. Both read `rows::Listing` after #86, so both read the
    /// model — and measured over 6,485 real rows they built the same shelf,
    /// once `Tree` was given a total order. One of them had to go, and the
    /// one that stayed is the one carrying #59's reissue ordering.
    pub fn from_members(
        librarian: &Librarian,
        members: HashSet<CopyId>,
        smart_artist_sort: bool,
    ) -> Self {
        Self::project(librarian, members, smart_artist_sort)
    }

    pub fn project(
        librarian: &Librarian,
        members: HashSet<CopyId>,
        smart_artist_sort: bool,
    ) -> Self {
        let mut lib = Shelf {
            members,
            ..Default::default()
        };

        // **This map used to be here and is now the identity** (#86). It went
        // from a copy's id to the files holding it, because a row was an arena
        // index and several could name one copy. A row *is* the copy now — the
        // durable id of its bytes — so two files with identical audio are one
        // row rather than two rows to be grouped, and the lookup below is
        // "is this copy one of ours".
        let tree = crate::model::tree::Tree::of(librarian, smart_artist_sort);
        let mut placed: HashSet<CopyId> = HashSet::new();
        for (artist_key, albums) in tree.artists() {
            for (album_key, album) in albums {
                for listing in &album.tracks {
                    let Some(here) = listing.copy.filter(|c| lib.members.contains(c)) else {
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
                    into.push(here);
                    placed.insert(here);
                }
            }
        }

        // **`file_by_tag` used to run here and is gone** (#86).
        //
        // It filed anything the model could not place under the names its tags
        // carried — the pre-#84 rule, kept as a fallback while the store and
        // the librarian were two sources that could disagree about what
        // existed. They are one source now: membership is asked of the same
        // seam the rows come from, so a row the model cannot place is not a
        // row. What is left of the case it covered is a track on no album,
        // and the tree files that under a stand-in rather than dropping it.
        lib.sorted = lib
            .tree
            .values()
            .flat_map(|albums| albums.values().flatten().copied())
            .collect();
        lib
    }

    pub fn len(&self) -> usize {
        self.members.len()
    }

    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
    }

    /// View 2: every track, in tree order.
    pub fn sorted(&self) -> &[CopyId] {
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

    pub fn artists(&self) -> impl Iterator<Item = (&SortKey, &BTreeMap<SortKey, Vec<CopyId>>)> {
        self.tree.iter()
    }

    pub fn artist_count(&self) -> usize {
        self.tree.len()
    }

    pub fn album_count(&self) -> usize {
        self.tree.values().map(|a| a.len()).sum()
    }

    pub fn contains(&self, id: CopyId) -> bool {
        self.members.contains(&id)
    }

    /// Which artist and album a track sits under.
    ///
    /// `aaa_mode` needs this to narrow the candidate pool, and getting it from
    /// the tree rather than the `Scan` keeps `next()`'s inputs to exactly what
    /// DESIGN §5 lists.
    pub fn locate(&self, id: CopyId) -> Option<(&SortKey, &SortKey)> {
        self.tree.iter().find_map(|(artist, albums)| {
            albums
                .iter()
                .find(|(_, tracks)| tracks.contains(&id))
                .map(|(album, _)| (artist, album))
        })
    }

    /// Every track by one artist, in album then disc/track order.
    pub fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId> {
        self.tree
            .get(artist)
            .map(|albums| albums.values().flatten().copied().collect())
            .unwrap_or_default()
    }

    /// One album's tracks, in disc/track order.
    pub fn album_tracks(&self, artist: &SortKey, album: &SortKey) -> &[CopyId] {
        self.tree
            .get(artist)
            .and_then(|albums| albums.get(album))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
}

// **`track_order` and `key_of` lived here and are gone with `reindex`** (#116).
//
// They ordered tracks inside one album — disc, then track number, then title
// — which is what makes a multi-disc album read as 1-01, 1-02, … 2-01 rather
// than interleaving the two discs' track 1s. `Tree::of` does it now, by the
// same key, so there is one answer rather than two that agreed by luck.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::scan::Holdings as Scan;
    use std::path::PathBuf;
    use std::time::Duration;

    fn t(path: &str, artist: Option<&str>, album: Option<&str>, title: Option<&str>) -> Track {
        Track {
            path: PathBuf::from(path),
            artist: artist.map(String::from),
            album: album.map(String::from),
            title: title.map(String::from),
            duration: Duration::from_secs(1),
            ..Default::default()
        }
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

        let l = crate::fixture::from_holdings(&store);
        let lib = Shelf::rebuild(&l, true);
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

        let l = crate::fixture::from_holdings(&store);
        let lib = Shelf::rebuild(&l, true);
        let titles: Vec<String> = lib
            .sorted()
            .iter()
            .map(|id| Listing::listed(&l, *id).expect("a row").title.to_string())
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

        let l = crate::fixture::from_holdings(&store);
        let lib = Shelf::rebuild(&l, true);
        assert_eq!(lib.artist_count(), 2);

        let unknown = lib
            .artists()
            .find(|(k, _)| k.as_str() == "unknown artist")
            .expect("untagged track should still appear");
        let album = unknown.1.values().next().unwrap();
        let row = Listing::listed(&l, album[0]).expect("a row");
        assert_eq!(row.title, "mystery.mp3");
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

        let l = crate::fixture::from_holdings(&store);
        let lib = Shelf::rebuild(&l, true);
        let titles: Vec<String> = lib
            .sorted()
            .iter()
            .map(|id| Listing::listed(&l, *id).expect("a row").title.to_string())
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

        let l = crate::fixture::from_holdings(&store);
        let lib = Shelf::rebuild(&l, true);
        let artists: Vec<&str> = lib.artists().map(|(k, _)| k.as_str()).collect();
        assert_eq!(artists, ["aardvarks", "zebra"]);
        // Same order in the flat view.
        let first = Listing::listed(&l, lib.sorted()[0]).expect("a row");
        assert_eq!(first.artist, Some("The Aardvarks"));
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

    /// A library holding these files, folded the way a launch would.
    ///
    /// **Ingested once.** This used to fold the scan, then rebuild the same
    /// library a second way through a `Track`-shaped copy — which silently
    /// dropped `mb_release_group`, so the fixture could not produce the
    /// grouping the test was about (#86).
    fn build(scan: &[Scanned]) -> (crate::model::seam::Librarian, Shelf) {
        let mut i = Instance::new(crate::fixture::device());
        i.ingest(100, scan);
        i.assert_holdings(
            100,
            scan.iter()
                .map(|s| {
                    (
                        crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&s.audio),
                        format!("/m/{}-{}.mp3", s.album.as_deref().unwrap_or(""), s.title),
                    )
                })
                .collect(),
        );
        let mut l = Librarian::from_catalogue(&i.catalogue());
        l.here(crate::fixture::device());

        let members: HashSet<CopyId> = Listing::rows(&l).into_iter().collect();
        let lib = Shelf::project(&l, members, false);
        (l, lib)
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

        // **Both rows show the group's title, and neither shows its own tag.**
        //
        // This used to prove the point by building the tag-grouped projection
        // beside the model-grouped one and showing two albums against one.
        // There is no tag-grouped projection any more (#86), and there is no
        // per-copy tag album either: a row's album *is* the release group's
        // title, which is what #84 decided. So what is asserted is that — one
        // title over two pressings whose sleeves disagree.
        let shown: std::collections::BTreeSet<String> = Listing::rows(&store)
            .iter()
            .filter_map(|id| Listing::listed(&store, *id).map(|r| r.album.to_string()))
            .collect();
        assert_eq!(
            shown.len(),
            1,
            "two pressings showed two album names: {shown:?}"
        );
        assert!(
            shown.contains("Luna") || shown.contains("Luna (Deluxe Edition)"),
            "and the name is one of the two the sleeves carry: {shown:?}"
        );
    }

    /// **A track on no album is still in the tree**, under a stand-in.
    ///
    /// This used to build a populated store beside an *empty* librarian and
    /// assert the `file_by_tag` fallback caught the difference. That state
    /// cannot occur any more and the fallback is gone (#86): membership is
    /// asked of the same seam the rows come from, so a row the model cannot
    /// place is not a row at all.
    ///
    /// What survives of the case is the one that was always the real one — a
    /// file whose tags name no album — and the answer is that it is filed
    /// rather than dropped.
    #[test]
    fn a_track_on_no_album_is_filed_rather_than_dropped() {
        let mut i = Instance::new(crate::model::id::Id::derived(b"laptop"));
        i.ingest(
            100,
            &[Scanned {
                album: None,
                ..scan("Mystery", "Nobody", None, "")
            }],
        );
        let l = Librarian::from_catalogue(&i.catalogue());

        let members: HashSet<CopyId> = crate::model::rows::Listing::rows(&l).into_iter().collect();
        assert_eq!(members.len(), 1, "the scan produced a row");

        let lib = Shelf::project(&l, members, false);
        assert_eq!(lib.sorted().len(), 1, "a track with no album vanished");
        assert_eq!(lib.artist_count(), 1);
    }
}

#[cfg(test)]
mod the_two_projections {
    //! **`project` and `from_members` must build the same shelf** — #116.
    //!
    //! They were a pair because one read the model and the other read the tags
    //! off a `Track`, and #84 chose between them. Both read `rows::Listing`
    //! now, so they should agree by construction — and "should" is not a thing
    //! to delete a projection on.

    use super::*;
    use crate::model::rows::Listing;

    /// Run deliberately: `cargo test --release -- --ignored projections`.
    #[test]
    #[ignore = "needs the real library; run deliberately"]
    fn they_build_the_same_shelf() {
        let root = std::path::PathBuf::from(std::env::var("HOME").unwrap()).join("MusicLibrary");
        if !root.exists() {
            eprintln!("no library at {}", root.display());
            return;
        }

        let mut held = crate::scan::Holdings::new();
        let report = crate::scan::scan(&mut held, std::slice::from_ref(&root));
        assert_eq!(report.errors.len(), 0);
        let l = crate::bridge::librarian(
            &held,
            &std::collections::BTreeSet::new(),
            crate::model::id::Id::derived(b"compare"),
            1_788_000_000_000,
        );

        let members: HashSet<CopyId> = Listing::rows(&l).into_iter().collect();
        let a = Shelf::project(&l, members.clone(), true);
        let b = Shelf::from_members(&l, members, true);

        eprintln!(
            "project   {} artists, {} albums, {} rows",
            a.artist_count(),
            a.album_count(),
            a.sorted().len()
        );
        eprintln!(
            "members   {} artists, {} albums, {} rows",
            b.artist_count(),
            b.album_count(),
            b.sorted().len()
        );

        assert_eq!(a.artist_count(), b.artist_count(), "artist nodes differ");
        assert_eq!(a.album_count(), b.album_count(), "album nodes differ");
        assert_eq!(a.sorted().len(), b.sorted().len(), "row counts differ");

        // Order too, not just membership: view 2 is the flattened tree and a
        // different order is a different library to look at.
        let differ = a
            .sorted()
            .iter()
            .zip(b.sorted())
            .filter(|(x, y)| x != y)
            .count();
        eprintln!(
            "order     {differ} of {} rows in a different place",
            a.sorted().len()
        );
        for (x, y) in a.sorted().iter().zip(b.sorted()).filter(|(x, y)| x != y) {
            let (rx, ry) = (
                Listing::listed(&l, *x).expect("a row"),
                Listing::listed(&l, *y).expect("a row"),
            );
            eprintln!(
                "  disc {:?} track {:?} on {:?} — {} vs {}",
                rx.disc, rx.track, rx.album, rx.title, ry.title
            );
        }
        assert_eq!(differ, 0, "the two projections order rows differently");
    }
}
