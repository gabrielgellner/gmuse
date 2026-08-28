//! The library projections — DESIGN §4.
//!
//! Views 1 and 2 share membership and differ only in shape: view 1 is an
//! artist → album → tracks tree, view 2 the same tracks as one sorted list.
//! Both are rebuilt from the `Store`, never mutated in place, which is what
//! makes `TrackId` indices worth having.

use std::collections::{BTreeMap, HashSet};

use crate::store::{Store, Track, TrackId};

/// A normalised sort key: case-folded, and with a leading article moved out of
/// the way when `smart_artist_sort` is on.
///
/// Kept separate from the display string, which stays on the `Track` — "The
/// Beatles" must sort under B and still render with its "The".
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SortKey(String);

impl SortKey {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Leading articles stripped for sorting. Lower-case; the input is folded first.
const ARTICLES: &[&str] = &["the ", "a ", "an "];

/// Build a sort key.
///
/// `smart` strips a leading article, matching cmus's `smart_artist_sort`. It is
/// applied to artists only — "The Wall" is an album whose T matters.
pub fn sort_key(s: &str, smart: bool) -> SortKey {
    let folded = s.trim().to_lowercase();
    if smart {
        for article in ARTICLES {
            if let Some(rest) = folded.strip_prefix(article) {
                let rest = rest.trim_start();
                if !rest.is_empty() {
                    return SortKey(rest.to_string());
                }
            }
        }
    }
    SortKey(folded)
}

/// Views 1 and 2. Same membership, two shapes.
#[derive(Debug, Default)]
pub struct Library {
    members: HashSet<TrackId>,
    tree: BTreeMap<SortKey, BTreeMap<SortKey, Vec<TrackId>>>,
    sorted: Vec<TrackId>,
    smart_artist_sort: bool,
}

// Some of this is ahead of its callers: the filter and command layers (M6,
// M8) are what will ask a library whether it contains a track.
#[allow(dead_code)]
impl Library {
    /// Rebuild both projections from every track in the store.
    pub fn rebuild(store: &Store, smart_artist_sort: bool) -> Self {
        let mut lib = Library {
            smart_artist_sort,
            ..Default::default()
        };
        for (id, _) in store.iter() {
            lib.members.insert(id);
        }
        lib.reindex(store);
        lib
    }

    fn reindex(&mut self, store: &Store) {
        self.tree.clear();
        self.sorted.clear();

        for &id in &self.members {
            let Some(track) = store.get(id) else { continue };
            let artist = sort_key(track.file_under(), self.smart_artist_sort);
            // Albums keep their article: "The Wall" belongs under T.
            let album = sort_key(track.album_or_unknown(), false);
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
    /// the tree rather than the `Store` keeps `next()`'s inputs to exactly what
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
fn track_order(store: &Store, a: TrackId, b: TrackId) -> std::cmp::Ordering {
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
        let mut store = Store::new();
        for (path, artist, title) in [
            ("/va/01.mp3", "Benjamin Tod", "The Mountain"),
            ("/va/02.mp3", "Sabine McCalla", "Baby, Please Don't Go"),
            ("/va/03.mp3", "Sierra Ferrell", "Years"),
        ] {
            let mut track = t(path, Some(artist), Some("YouTube Sessions"), Some(title));
            track.albumartist = Some("Various Artists".into());
            store.upsert(track);
        }

        let lib = Library::rebuild(&store, true);
        // One artist, not three: the whole point of the albumartist rule.
        assert_eq!(lib.artist_count(), 1);
        assert_eq!(lib.album_count(), 1);
        let (artist, albums) = lib.artists().next().unwrap();
        assert_eq!(artist.as_str(), "various artists");
        assert_eq!(albums.values().next().unwrap().len(), 3);
    }

    #[test]
    fn multi_disc_albums_order_by_disc_then_track() {
        let mut store = Store::new();
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

        let lib = Library::rebuild(&store, true);
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
        let mut store = Store::new();
        store.upsert(t("/m/mystery.mp3", None, None, None));
        store.upsert(t(
            "/m/known.mp3",
            Some("Burial"),
            Some("Untrue"),
            Some("Archangel"),
        ));

        let lib = Library::rebuild(&store, true);
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
        let mut store = Store::new();
        let mut a = t("/m/a.mp3", Some("X"), Some("Y"), Some("no number"));
        a.tracknumber = None;
        let mut b = t("/m/b.mp3", Some("X"), Some("Y"), Some("number 9"));
        b.tracknumber = Some(9);
        store.upsert(a);
        store.upsert(b);

        let lib = Library::rebuild(&store, true);
        let titles: Vec<&str> = lib
            .sorted()
            .iter()
            .map(|id| store.get(*id).unwrap().display_title())
            .collect();
        assert_eq!(titles, ["number 9", "no number"]);
    }

    #[test]
    fn view_two_is_the_tree_flattened() {
        let mut store = Store::new();
        store.upsert(t("/m/1.mp3", Some("Zebra"), Some("Album"), Some("z")));
        store.upsert(t(
            "/m/2.mp3",
            Some("The Aardvarks"),
            Some("Album"),
            Some("a"),
        ));

        let lib = Library::rebuild(&store, true);
        let artists: Vec<&str> = lib.artists().map(|(k, _)| k.as_str()).collect();
        assert_eq!(artists, ["aardvarks", "zebra"]);
        // Same order in the flat view.
        let first = store.get(lib.sorted()[0]).unwrap();
        assert_eq!(first.artist.as_deref(), Some("The Aardvarks"));
        assert_eq!(lib.sorted().len(), 2);
    }
}
