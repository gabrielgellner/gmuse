//! Smart playlists — DESIGN §7, issue #15.
//!
//! A named filter expression with an order, behaving as a **play source**
//! rather than as a narrowing of the library. That distinction is the whole
//! design, and it is not the obvious one, so it is worth stating outright:
//!
//! Saving is *not* what makes something a source — catalogs are saved too, and
//! they narrow. The test is whether the rest of the library is still reachable
//! while this thing is active. A catalog hides everything outside it; a
//! playlist sits alongside the library and is simply one more place to play
//! from. So a smart playlist fills view 3 and never touches the `Shelf`
//! projection.
//!
//! That also settles what would otherwise be two mechanisms for one idea: a
//! saved expression that *narrows* already exists, and it is spelled
//! `[catalogs.x] where = "…"`. The same expression can be written as either,
//! and which you want depends on the verb — *show me only my jazz* is a
//! catalog, *play my highly rated* is a smart playlist.
//!
//! Membership is evaluated against the whole [`Scan`], not the active
//! `Shelf`. §4 is explicit that a playlist "must not be filtered by the
//! active set" because crossing themes is the point of one; a smart playlist
//! inherits that rather than being a special case.

use crate::filter::Filter;
use crate::model::rows::Listing;
use crate::sort::{Sort, SortEnv};
use crate::store::TrackId;

#[derive(Debug, Clone, PartialEq)]
pub struct SmartPlaylist {
    pub name: String,
    /// Which tracks. Required — a smart playlist with no rule is the library.
    pub filter: Filter,
    /// What order. Defaults to [`Sort::library_order`].
    pub sort: Sort,
}

impl SmartPlaylist {
    /// The tracks, in order.
    ///
    /// Recomputed rather than stored, so the list cannot drift out of step
    /// with the history behind it — a rule mentioning `play_count` has to
    /// answer differently after you play something, and a materialised list
    /// would need invalidating from every corner of the app instead.
    pub fn members(&self, rows: &dyn Listing, all: &[TrackId], env: SortEnv<'_>) -> Vec<TrackId> {
        let matches = self.filter.compile(env.env);
        let mut ids: Vec<TrackId> = all
            .iter()
            .copied()
            .filter(|id| rows.subject(*id).is_some_and(|s| matches(&s)))
            .collect();
        self.sort.apply(&mut ids, rows, env);
        ids
    }

    /// What the config said, for a message and for view 3's title.
    pub fn rule_label(&self) -> String {
        format!("{}  sorted by  {}", self.filter.source, self.sort.source)
    }
}

/// The defined set, in config order.
#[derive(Debug, Clone, Default)]
pub struct SmartPlaylists {
    defined: Vec<SmartPlaylist>,
}

impl SmartPlaylists {
    pub fn define(&mut self, name: &str, filter: Filter, sort: Option<Sort>) {
        let entry = SmartPlaylist {
            name: name.to_string(),
            filter,
            sort: sort.unwrap_or_else(Sort::library_order),
        };
        match self.defined.iter_mut().find(|p| p.name == name) {
            Some(slot) => *slot = entry,
            None => self.defined.push(entry),
        }
    }

    pub fn get(&self, name: &str) -> Option<&SmartPlaylist> {
        self.defined.iter().find(|p| p.name == name)
    }

    /// Every rule, in config order.
    pub fn all(&self) -> impl Iterator<Item = &SmartPlaylist> {
        self.defined.iter()
    }

    pub fn names(&self) -> Vec<&str> {
        self.defined.iter().map(|p| p.name.as_str()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filter::Env;
    use crate::model::rows::Listing;
    use crate::sort::Sort;
    use crate::store::{Scan, Track};
    use std::path::PathBuf;
    use std::time::Duration;

    fn store() -> Scan {
        let mut s = Scan::new();
        for (n, (path, artist, genre, year)) in [
            ("/m/General/a.mp3", "Abba", "pop", 1976),
            ("/m/General/b.mp3", "Burial", "electronic", 2007),
            ("/m/Dance/c.mp3", "Caribou", "electronic", 2020),
        ]
        .into_iter()
        .enumerate()
        {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some(artist.into()),
                title: Some(artist.into()),
                genres: vec![genre.into()],
                date: Some(year),
                duration: Duration::from_secs(200),
                // History hangs off an identity now, so a test track needs one.
                audio_id: Some(crate::audio_id::AudioId {
                    scheme: 1,
                    hash: n as u128 + 1,
                }),
                ..Default::default()
            });
        }
        s
    }

    /// The library's answers about listening, built by hand.
    ///
    /// A real `Catalogue` rather than a stub, because it is the thing the
    /// player actually asks and its fields are the fold's own output.
    fn listening() -> crate::model::log::Catalogue {
        crate::model::log::Catalogue::default()
    }

    /// Note a play against whatever recording `path` is, the way the log would.
    fn played(cat: &mut crate::model::log::Catalogue, s: &Scan, path: &str, at: i64) {
        let id = s.id_for_path(&PathBuf::from(path)).expect("a known track");
        let recording = s.recording_of(id).expect("with an identity");
        *cat.plays.entry(recording).or_insert(0) += 1;
        cat.last_played.insert(recording, at);
    }

    fn env(h: &crate::model::log::Catalogue) -> SortEnv<'_> {
        SortEnv {
            env: Env {
                listened: Some(h),
                now: 1_000_000,
            },
            smart_artist_sort: false,
            seed: 1,
        }
    }

    fn playlist(where_: &str, sort: Option<&str>) -> SmartPlaylist {
        let mut set = SmartPlaylists::default();
        set.define(
            "p",
            crate::filter::Filter::parse(where_).unwrap(),
            sort.map(|s| Sort::parse(s).unwrap()),
        );
        set.get("p").unwrap().clone()
    }

    fn names(p: &SmartPlaylist, s: &Scan, e: SortEnv<'_>) -> Vec<String> {
        p.members(s, &Listing::rows(s), e)
            .iter()
            .map(|id| s.get(*id).unwrap().artist.clone().unwrap())
            .collect()
    }

    #[test]
    fn members_are_the_rule_in_the_given_order() {
        let (s, h) = (store(), listening());
        let p = playlist("genre=\"electronic\"", Some("-year"));
        assert_eq!(names(&p, &s, env(&h)), ["Caribou", "Burial"]);
    }

    #[test]
    fn an_omitted_sort_is_the_library_order() {
        let (s, h) = (store(), listening());
        let p = playlist("year>0", None);
        assert_eq!(p.sort, Sort::library_order());
        assert_eq!(names(&p, &s, env(&h)), ["Abba", "Burial", "Caribou"]);
    }

    /// §4 says a playlist "must not be filtered by the active set", because
    /// crossing themes is the point of one. A smart playlist inherits that: it
    /// draws from the whole `Scan`, so a rule can reach a track no active
    /// catalog would show.
    #[test]
    fn membership_crosses_catalogs() {
        let (s, h) = (store(), listening());
        let p = playlist("genre=\"electronic\"", Some("artist"));
        // One lives under General and one under Dance; both are here.
        let paths: Vec<String> = p
            .members(&s, &Listing::rows(&s), env(&h))
            .iter()
            .map(|id| s.get(*id).unwrap().path.display().to_string())
            .collect();
        assert_eq!(paths, ["/m/General/b.mp3", "/m/Dance/c.mp3"]);
    }

    #[test]
    fn a_rule_that_matches_nothing_is_empty_rather_than_everything() {
        let (s, h) = (store(), listening());
        let p = playlist("genre=\"polka\"", None);
        assert!(p.members(&s, &Listing::rows(&s), env(&h)).is_empty());
    }

    /// The rule is re-read on every call, so a play or a rating changes the
    /// answer without anything having to invalidate a cached list.
    #[test]
    fn history_rules_answer_differently_as_history_changes() {
        let s = store();
        let mut h = listening();
        let p = playlist("play_count>0", None);
        assert!(p.members(&s, &Listing::rows(&s), env(&h)).is_empty());

        played(&mut h, &s, "/m/General/b.mp3", 900_000);
        assert_eq!(names(&p, &s, env(&h)), ["Burial"]);
    }

    #[test]
    fn defining_the_same_name_twice_replaces_it() {
        let mut set = SmartPlaylists::default();
        set.define("p", crate::filter::Filter::parse("year>0").unwrap(), None);
        set.define(
            "p",
            crate::filter::Filter::parse("year>2000").unwrap(),
            None,
        );
        assert_eq!(set.names(), ["p"]);
        assert_eq!(set.get("p").unwrap().filter.source, "year>2000");
    }

    #[test]
    fn the_label_names_both_halves() {
        let p = playlist("rating>=4", Some("-rating, artist"));
        assert_eq!(p.rule_label(), "rating>=4  sorted by  -rating, artist");
    }
}
