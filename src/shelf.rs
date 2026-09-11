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
pub use crate::model::sortkey::SortKey;

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
    /// **How many tracks the record has, where the row is short of them** —
    /// #137. Absent when the album is complete, when nothing counted it, or
    /// when the group's copies come off more than one pressing.
    album_totals: BTreeMap<(SortKey, SortKey), u16>,
    /// Which pressing a tree node's copies come off, or `None` where they come
    /// off more than one — #137. A group spanning editions has no single
    /// count, and saying one would print halves that do not belong together.
    album_pressing:
        BTreeMap<(SortKey, SortKey), Option<crate::model::id::Id<crate::model::entity::Release>>>,
    /// **Which disc of its record each row sits on** — #169.
    ///
    /// A five-disc *Let It Be* has five track 1s, and drawn flat they sit
    /// adjacent under identical numbering, which reads as the library holding
    /// one song five times. The model has always known better — `place_of`
    /// returns `(disc, track)` and the tree already sorts by it — so what was
    /// missing was never the ordering, only a row to say where one disc ends.
    ///
    /// Absent for a copy nothing placed, which is an ordinary row.
    discs: BTreeMap<CopyId, u16>,
    /// The discs an album's *held* copies span, which is not how many the
    /// record has. A two-disc set you own half of spans one, and drawing a
    /// disc header over the only disc present would be a level that opens onto
    /// everything under it — #135's rule, one level up.
    album_discs: BTreeMap<(SortKey, SortKey), std::collections::BTreeSet<u16>>,
    /// `CD`, `12" Vinyl`, `Digital Media` — what the authority called this
    /// disc, where it said. Carried because a header reading `Disc 2 · CD`
    /// tells you what the sleeve does and a bare number does not.
    disc_formats: BTreeMap<(SortKey, SortKey, u16), String>,
    sorted: Vec<CopyId>,
    /// Which two fields this was grouped by (#97).
    levels: Levels,
}

/// What a view is grouped by.
///
/// **Two fields, and the outer one may be multi-valued.** A row has one artist
/// and many genres, so grouping by genre files a copy under each of them —
/// which is what you want, and which means every count over the tree has to be
/// distinct rather than a sum. `tree-view-design.md` §6 named this before it
/// could happen; grouping by a chosen field is what made it possible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Levels {
    /// No grouping — one row per track, which is what view 2 was.
    ///
    /// **`levels: []` in `query-language.md` §10**, and the reason views 1 and
    /// 2 were "one set drawn two ways": nothing about the *membership*
    /// differed, only whether it was grouped.
    Flat,
    /// Grouped by two fields. `[artist, album]` is the tree as it has always
    /// been, said out loud rather than hardcoded.
    By { outer: Level, inner: Level },
}

/// One level of the tree: a field, and optionally where in its hierarchy.
///
/// **`genre@1` is what the `Broad; Style` tag convention imitates** — issue
/// #79, `domain-model.md` §5. The convention exists because a view could not
/// say "group by the broad term, then by the specific one"; a level that can
/// name a depth is that sentence, and the compound string stops being
/// load-bearing.
///
/// Depth is **1-based from the top**, as the issue writes it: `genre@1` is the
/// broadest ancestor, `genre@2` the one under it. A concept shallower than the
/// level asked for stands for itself rather than vanishing — the ragged case,
/// and a row that dropped out of the tree because its genre had no parent
/// would be a worse answer than a repeated one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Level {
    pub field: crate::query::format::Field,
    /// `None` is the value itself — every level before #79 was this.
    pub at: Option<u8>,
}

impl Level {
    pub fn of(field: crate::query::format::Field) -> Self {
        Level { field, at: None }
    }

    /// How it would be typed, so `:view levels …` round-trips.
    pub fn written(self) -> String {
        match self.at {
            None => self.field.name(),
            Some(n) => format!("{}@{n}", self.field.name()),
        }
    }
}

impl Default for Levels {
    fn default() -> Self {
        Levels::By {
            outer: Level::of(crate::query::format::Field::FileUnder),
            inner: Level::of(crate::query::format::Field::Album),
        }
    }
}

impl Levels {
    /// The pair to group by, or `None` when flat.
    pub fn pair(self) -> Option<(Level, Level)> {
        match self {
            Levels::Flat => None,
            Levels::By { outer, inner } => Some((outer, inner)),
        }
    }

    pub fn is_flat(self) -> bool {
        matches!(self, Levels::Flat)
    }

    /// How it would be typed, so `:view levels …` round-trips.
    pub fn written(self) -> String {
        match self {
            Levels::Flat => "flat".to_string(),
            Levels::By { outer, inner } => format!("{},{}", outer.written(), inner.written()),
        }
    }
    /// Whether the outer level can put one row in more than one node.
    ///
    /// The **declared** answer, not a guess: a level is single-valued unless
    /// it says otherwise, and this is the flag `tree-view-design.md` §6 said
    /// must exist before a multi-valued grouping does.
    pub fn outer_is_multi(self) -> bool {
        matches!(
            self,
            Levels::By {
                outer: Level {
                    field: crate::query::format::Field::Facet(_),
                    ..
                },
                ..
            }
        )
    }
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
        Self::grouped(librarian, members, Levels::default(), smart_artist_sort)
    }

    /// The same, grouped by two chosen fields — #97.
    ///
    /// **`levels` is a value, not a shape somebody hardcoded.** Views 1 and 2
    /// were one set drawn two ways; `[artist, album]` is the tree as it was,
    /// and `[genre, album]` is a grouping that could not be expressed at all.
    pub fn grouped(
        librarian: &Librarian,
        members: HashSet<CopyId>,
        levels: Levels,
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
        lib.levels = levels;
        // **Flat is still grouped underneath**, by the default pair, because
        // the flat list is the tree read in order — `sorted` is built from it
        // below. What flat changes is how it is *drawn*, which is the whole
        // point of `levels` being presentation (#97).
        let pair = levels
            .pair()
            .or_else(|| Levels::default().pair())
            .expect("the default is grouped");
        // **The tree is built over the members** (#121). It used to be built
        // over the whole library and filtered here, so narrowing to 786 rows
        // cost what drawing all 6,485 did — and every `:view` paid it again.
        let tree =
            crate::model::tree::Tree::grouped(librarian, pair, smart_artist_sort, &lib.members);
        let mut placed: HashSet<CopyId> = HashSet::new();
        for (artist_key, albums) in tree.artists() {
            for (album_key, album) in albums {
                for listing in &album.tracks {
                    // **The row is the appearance** (#155); the copy filling
                    // it is a property, and the pressing question is about it.
                    let here = listing.row;
                    lib.artist_names
                        .entry(artist_key.clone())
                        .or_insert_with(|| tree.artist_name(artist_key).to_string());
                    lib.album_names
                        .entry((artist_key.clone(), album_key.clone()))
                        .or_insert_with(|| album.title.clone());
                    // **Which pressing this group's copies come off** (#137).
                    // A tree node is a release *group* — the album across its
                    // editions — and the count is a fact about one pressing.
                    // Copies off two of them have no single answer, so the
                    // group is marked mixed and says nothing rather than
                    // printing halves that do not belong together.
                    // **Which disc, from the model's own answer** (#169).
                    // `place_of` is what the tree already sorts by, so the
                    // header and the order cannot disagree about where a disc
                    // starts.
                    let (disc, _) = librarian.place_of(here.slot());
                    if disc != u16::MAX {
                        lib.discs.insert(here, disc);
                        lib.album_discs
                            .entry((artist_key.clone(), album_key.clone()))
                            .or_default()
                            .insert(disc);
                        if let Some(what) = librarian.medium_format(here.slot()) {
                            lib.disc_formats
                                .insert((artist_key.clone(), album_key.clone(), disc), what);
                        }
                    }
                    let seen = librarian
                        .shown_copy(here)
                        .and_then(|copy| librarian.release_of(librarian.placed_at(copy)));
                    lib.album_pressing
                        .entry((artist_key.clone(), album_key.clone()))
                        .and_modify(|known| {
                            if *known != seen {
                                *known = None;
                            }
                        })
                        .or_insert(seen);

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
        // **The counts, once the pressings are settled** (#137). Only where
        // the record is short: `12 of 12` would be noise on 5,577 of the
        // 6,485 files here, and an uncounted record has nothing to be short
        // of.
        for (key, pressing) in &lib.album_pressing {
            if let Some(release) = pressing
                && librarian.held_of(*release) == crate::model::seam::Held::Partial
                && let Some((_, total)) = librarian.counted_of(*release)
            {
                lib.album_totals.insert(key.clone(), total);
            }
        }

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

    /// How many tracks the record has, when the row is short of them — #137.
    pub fn album_total(&self, artist: &SortKey, album: &SortKey) -> Option<u16> {
        self.album_totals
            .get(&(artist.clone(), album.clone()))
            .copied()
    }

    /// Which disc a row sits on, where anything placed it — #169.
    pub fn disc_of(&self, id: CopyId) -> Option<u16> {
        self.discs.get(&id).copied()
    }

    /// **How many discs this album's held copies span** — #169, and held is
    /// the operative word. One means no header: a level that opens onto
    /// everything beneath it costs a keystroke and says nothing, which is
    /// #135's rule applied to discs instead of tracks.
    pub fn album_discs(&self, artist: &SortKey, album: &SortKey) -> usize {
        self.album_discs
            .get(&(artist.clone(), album.clone()))
            .map_or(0, std::collections::BTreeSet::len)
    }

    /// What the authority called this disc, where it said — #169.
    pub fn disc_format(&self, artist: &SortKey, album: &SortKey, disc: u16) -> Option<String> {
        self.disc_formats
            .get(&(artist.clone(), album.clone(), disc))
            .cloned()
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

    /// How this was grouped.
    pub fn levels(&self) -> Levels {
        self.levels
    }

    /// **Distinct copies, not a sum over nodes** — `tree-view-design.md` §6.
    ///
    /// A multi-valued outer level files one copy under several nodes, so
    /// adding up the nodes counts it twice. That is the difference between "a
    /// subtree reports 42 tracks" and "it enqueues 37", and the two must not
    /// be allowed to disagree.
    pub fn distinct(&self, ids: &[CopyId]) -> usize {
        if !self.levels.outer_is_multi() {
            return ids.len();
        }
        ids.iter().collect::<HashSet<_>>().len()
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
        self.everywhere(id).into_iter().next()
    }

    /// **Every node a copy sits under** — §6's third call site.
    ///
    /// Revealing a track grouped by genre has to highlight it under each of
    /// its genres, or "go to what is playing" lands on one arbitrary node and
    /// looks like the others do not hold it.
    pub fn everywhere(&self, id: CopyId) -> Vec<(&SortKey, &SortKey)> {
        self.tree
            .iter()
            .flat_map(|(artist, albums)| {
                albums
                    .iter()
                    .filter(move |(_, tracks)| tracks.contains(&id))
                    .map(move |(album, _)| (artist, album))
            })
            .collect()
    }

    /// Every track under one node, in sub-node then disc/track order.
    ///
    /// **Deduplicated when the level is multi-valued** (§6): enqueueing a
    /// genre must not queue a track twice because it carries two genres, and
    /// the count beside the node has to be the same number.
    pub fn artist_tracks(&self, artist: &SortKey) -> Vec<CopyId> {
        let mut out: Vec<CopyId> = self
            .tree
            .get(artist)
            .map(|albums| albums.values().flatten().copied().collect())
            .unwrap_or_default();
        if self.levels.outer_is_multi() {
            let mut seen = HashSet::new();
            out.retain(|id| seen.insert(*id));
        }
        out
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
    // **From where it lives, not from this module's re-export** — #191. The
    // query layer sorts by it and had been reaching up through `shelf` to get
    // at something that sits below both, which the boundary test caught.
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::model::sortkey::sort_key;
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

    /// **The tree is grouped by chosen fields** — #97's headline, and the
    /// thing that could not be expressed before: `levels: [genre]`.
    #[test]
    fn levels_choose_what_the_tree_groups_by() {
        let mut techno = scan("Callisto", "Bodzin", Some("Bodzin"), "Luna");
        techno.genres = vec!["Techno".into()];
        let mut folk = scan("The Sea", "Ferrell", Some("Ferrell"), "Long Time Coming");
        folk.genres = vec!["Folk".into()];

        let mut i = Instance::new(crate::fixture::device());
        i.ingest(100, &[techno.clone(), folk.clone()]);
        i.assert_holdings(
            100,
            [&techno, &folk]
                .iter()
                .map(|s| {
                    (
                        crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&s.audio),
                        format!("/m/{}.mp3", s.title),
                    )
                })
                .collect(),
        );
        let mut l = Librarian::from_catalogue(&i.catalogue());
        l.here(crate::fixture::device());
        let members: HashSet<CopyId> = Listing::rows(&l).into_iter().collect();

        let by_artist = Shelf::grouped(&l, members.clone(), Levels::default(), false);
        assert_eq!(by_artist.artist_count(), 2, "two artists");

        let genre = crate::query::format::Field::parse("genre").unwrap();
        let by_genre = Shelf::grouped(
            &l,
            members,
            Levels::By {
                outer: Level::of(genre),
                inner: Level::of(crate::query::format::Field::Album),
            },
            false,
        );
        let names: Vec<&str> = by_genre
            .artists()
            .map(|(k, _)| by_genre.artist_name(k))
            .collect();
        assert_eq!(names, ["Folk", "Techno"], "not grouped by genre");
    }

    /// **`genre@1` groups by the broad term** — #79, and the sentence the
    /// `Broad; Style` tag convention was imitating in a string.
    ///
    /// Two rows tagged `Techno` and `House`, both narrower-of `Electronic`,
    /// are one node at level 1 and two at level 2. That is the whole shape
    /// `domain-model.md` §5 said a view could not express.
    #[test]
    fn a_level_can_name_a_depth_in_the_genre_hierarchy() {
        let mut techno = scan("Callisto", "Bodzin", Some("Bodzin"), "Luna");
        techno.genres = vec!["Techno".into()];
        let mut house = scan("The Sea", "Ferrell", Some("Ferrell"), "Long Time Coming");
        house.genres = vec!["House".into()];

        let mut i = Instance::new(crate::fixture::device());
        i.ingest(100, &[techno.clone(), house.clone()]);
        i.assert_holdings(
            100,
            [&techno, &house]
                .iter()
                .map(|s| {
                    (
                        crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&s.audio),
                        format!("/m/{}.mp3", s.title),
                    )
                })
                .collect(),
        );
        // The hierarchy is curated, not read off the tags — which is the point:
        // once it exists, the compound string stops being load-bearing.
        let h = crate::model::id::Id::<crate::model::entity::Hierarchy>::derived(b"browse");
        i.narrows(100, h, crate::model::facet::GENRE, "Electronic", "Techno");
        i.narrows(100, h, crate::model::facet::GENRE, "Electronic", "House");

        let mut l = Librarian::from_catalogue(&i.catalogue());
        l.here(crate::fixture::device());
        let members: HashSet<CopyId> = Listing::rows(&l).into_iter().collect();

        let genre = crate::query::format::Field::parse("genre").unwrap();
        let names = |at: Option<u8>| {
            let shelf = Shelf::grouped(
                &l,
                members.clone(),
                Levels::By {
                    outer: Level { field: genre, at },
                    inner: Level::of(crate::query::format::Field::Album),
                },
                false,
            );
            shelf
                .artists()
                .map(|(k, _)| shelf.artist_name(k).to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(names(Some(1)), ["Electronic"], "the broad term is one node");
        // **Two parents put the row under both**, because genre is many-valued
        // and picking one would be an arbitrary tie-break deciding where a row
        // appears. `Techno` is filed under `Rock` as well, absurdly but
        // decisively — and level 1 now says so rather than choosing.
        i.narrows(100, h, crate::model::facet::GENRE, "Rock", "Techno");
        let mut l2 = Librarian::from_catalogue(&i.catalogue());
        l2.here(crate::fixture::device());
        let members2: HashSet<CopyId> = Listing::rows(&l2).into_iter().collect();
        let shelf = Shelf::grouped(
            &l2,
            members2,
            Levels::By {
                outer: Level {
                    field: genre,
                    at: Some(1),
                },
                inner: Level::of(crate::query::format::Field::Album),
            },
            false,
        );
        let mut both: Vec<String> = shelf
            .artists()
            .map(|(k, _)| shelf.artist_name(k).to_string())
            .collect();
        both.sort();
        assert_eq!(both, ["Electronic", "Rock"], "a second parent was dropped");

        assert_eq!(names(Some(2)), ["House", "Techno"], "and level 2 is both");
        assert_eq!(names(None), ["House", "Techno"], "no depth is the value");
    }

    /// **A multi-valued level files one copy under several nodes, and every
    /// count over the tree has to know** — `tree-view-design.md` §6, named
    /// before it could happen and true the moment grouping became a choice.
    ///
    /// Without this "a subtree reports 42 tracks and enqueues 37".
    #[test]
    fn a_track_with_two_genres_is_counted_once_and_enqueued_once() {
        let mut both = scan("Callisto", "Bodzin", Some("Bodzin"), "Luna");
        both.genres = vec!["Techno".into(), "Electronic".into()];

        let mut i = Instance::new(crate::fixture::device());
        i.ingest(100, std::slice::from_ref(&both));
        i.assert_holdings(
            100,
            [(
                crate::model::id::Id::<crate::model::entity::Encoding>::of_audio(&both.audio),
                "/m/callisto.mp3".to_string(),
            )]
            .into_iter()
            .collect(),
        );
        let mut l = Librarian::from_catalogue(&i.catalogue());
        l.here(crate::fixture::device());
        let members: HashSet<CopyId> = Listing::rows(&l).into_iter().collect();
        let id = *members.iter().next().unwrap();

        let levels = Levels::By {
            outer: Level::of(crate::query::format::Field::parse("genre").unwrap()),
            inner: Level::of(crate::query::format::Field::Album),
        };
        assert!(levels.outer_is_multi(), "genre must declare itself multi");
        let lib = Shelf::grouped(&l, members, levels, false);

        // Under both, which is the point of grouping by a multi-valued field.
        assert_eq!(lib.artist_count(), 2, "one track, two genres, two nodes");
        assert_eq!(
            lib.everywhere(id).len(),
            2,
            "revealing it must find both places it sits"
        );

        // **But counted once and enqueued once.** The two numbers may not
        // disagree, which is exactly the failure §6 predicted.
        let flat: Vec<CopyId> = lib
            .artists()
            .flat_map(|(_, albums)| albums.values().flatten().copied())
            .collect();
        assert_eq!(flat.len(), 2, "the tree really does hold it twice");
        assert_eq!(lib.distinct(&flat), 1, "counted twice");

        let queued = lib.artist_tracks(lib.artists().next().unwrap().0);
        assert_eq!(queued.len(), 1, "enqueued twice");
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
        let l = crate::bridge::imported(
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

#[cfg(test)]
mod perf {
    //! What it costs to draw a view — issue #121.
    //!
    //! `just bench-shelf`. The resampler has had a bench since the beginning
    //! and the library path had none, so the only signal that drawing a view
    //! had become slow was somebody noticing that it felt slow. It had: on a
    //! 6,485-row library `:view in="Dance"` spent **1.3 seconds** in
    //! `Shelf::grouped`, and the cost did not depend on how many rows matched.
    //!
    //! Synthetic rather than the machine's own library, because the numbers
    //! have to mean the same thing on two machines a week apart.
    //!
    //! Three phases, kept apart because they fail differently: **resolve** is
    //! the filter, **copies** turns recordings into rows, and **shelf** groups
    //! them into the tree. The one that went wrong was the third, and a single
    //! end-to-end number would have hidden which.

    use std::collections::HashSet;
    use std::time::Instant;

    use super::*;
    use crate::model::instance::{Instance, Scanned};

    const GENRES: [&str; 6] = ["Techno", "House", "Folk", "Rock", "Jazz", "Ambient"];

    /// `albums` albums of ten tracks over `artists` artists — the shape of a
    /// real library at the size the slowness showed up at.
    fn library(artists: usize, albums: usize) -> Librarian {
        let mut scan = Vec::new();
        for a in 0..albums {
            for track in 1..=10u16 {
                let artist = format!("Artist {:04}", a % artists);
                scan.push(Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: (a as u128) * 1000 + u128::from(track),
                    },
                    title: format!("Track {track:02} of {a:04}"),
                    artist: artist.clone(),
                    album: Some(format!("Album {a:04}")),
                    album_artist: Some(artist),
                    genres: vec![GENRES[a % GENRES.len()].to_string()],
                    disc: Some(1),
                    track: Some(track),
                    // **The totals, because the real library carries them**
                    // (#134): 88% of it does, and a bench whose releases
                    // cannot be counted never runs the counting.
                    track_total: Some(10),
                    disc_total: Some(1),
                    length_ms: 200_000,
                    ..Default::default()
                });
            }
        }
        let mut log = Instance::new(crate::fixture::device());
        log.ingest(100, &scan);
        let mut l = Librarian::from_catalogue(&log.catalogue());
        l.here(crate::fixture::device());
        l
    }

    /// The table, and the bound that makes it a test rather than a report.
    ///
    /// **Generous on purpose.** A tight budget on a shared machine is a flaky
    /// test, and what this has to catch is not a 20% drift — it is the shape
    /// of the bug it was written for, where narrowing to a tenth of the
    /// library cost what drawing all of it did. Fifty milliseconds is roughly
    /// forty times what a narrowed view takes now and still a fiftieth of what
    /// it took before the fix.
    #[test]
    fn drawing_a_view_scales_with_what_it_drew() {
        let l = library(200, 650);
        let rows = Listing::rows(&l).len();
        let listened = crate::model::log::Catalogue::default();
        let env = crate::query::filter::Env {
            listened: Some(&listened),
            named: None,
            now: 1_700_000_000,
        };

        eprintln!("\n{rows} rows\n");
        eprintln!(
            "{:<28} {:>9} {:>9} {:>9} {:>7}",
            "query", "resolve", "copies", "shelf", "rows"
        );
        // **Per drawn row, against the whole library's own rate.** The bound
        // used to be a wall clock — 50ms — and a wall clock measures the
        // machine as much as the code. Under `cargo test` this runs beside
        // 1,300 other tests and takes 52-58ms for the contention alone, where
        // on its own it takes 46-48; so `just ci` failed at random and the one
        // safe reading of a red bar was "look again".
        //
        // The claim was never about milliseconds. #121 is that drawing a view
        // cost the same whatever the filter matched — 1.3 seconds for a
        // handful of rows out of 6,485 — so what has to hold is that the cost
        // *follows the rows*. A ratio says that, and contention cancels out of
        // it because it inflates both sides equally.
        let mut full: Option<(std::time::Duration, usize)> = None;
        let mut worst: Option<(f64, &str, usize)> = None;
        for src in [
            "all",
            r#"genre="Techno""#,
            r#"artist="Artist 0001""#,
            r#"genre="Techno" &artist="Artist 0001""#,
        ] {
            let stack = crate::view::Stack::parse(src).expect("the bench's own queries parse");

            let t = Instant::now();
            let resolved = stack.resolve(&l, &listened, env);
            let resolve = t.elapsed();

            let t = Instant::now();
            let members: HashSet<CopyId> = resolved.copies(&l);
            let copies = t.elapsed();
            let n = members.len();

            let t = Instant::now();
            let shelf = Shelf::grouped(&l, members, Levels::default(), true);
            let grouped = t.elapsed();

            eprintln!(
                "{src:<28} {resolve:>9.1?} {copies:>9.1?} {grouped:>9.1?} {:>7}",
                shelf.len()
            );
            assert_eq!(shelf.len(), n, "the shelf lost rows the filter matched");

            match full {
                // "all" is first, and is the rate everything else is read
                // against: what this library costs per row when nothing is
                // filtered out.
                None => full = Some((grouped, n)),
                Some((whole, all_rows)) => {
                    let rate = whole.as_secs_f64() / all_rows as f64;
                    if n == 0 {
                        // Nothing drawn, so there is no rate to compare — what
                        // must hold is that it did not walk the library to
                        // find that out. Measured at 1.1µs against 331ms.
                        assert!(
                            grouped.as_secs_f64() < whole.as_secs_f64() / 10.0,
                            "a query matching nothing took {grouped:?} against {whole:?} for \
                             the whole library — it is walking rows it will not draw"
                        );
                    } else {
                        let mine = grouped.as_secs_f64() / n as f64;
                        if worst.is_none_or(|(w, ..)| mine / rate > w) {
                            worst = Some((mine / rate, src, n));
                        }
                    }
                }
            }
        }

        let (whole, all_rows) = full.expect("the bench asks for `all` first");
        let (ratio, src, n) = worst.expect("the bench asks for narrowed views too");
        // **Four times the full library's rate**, which is loose on purpose:
        // the numbers sit within 15% of each other in practice (51, 48 and
        // 45 microseconds a row), and the regression this exists for is not
        // subtle — drawing 1,090 rows at the whole library's *total* cost is
        // six times the rate, and 40 rows at it is a hundred and eighty.
        assert!(
            ratio < 4.0,
            "{src:?} drew {n} of {all_rows} rows at {ratio:.1}x the rate the whole library \
             manages ({whole:?} for all of it) — it is paying for rows it did not draw"
        );
    }
}
