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
//!
//! ## Two places a rule is written
//!
//! A `[playlists.x]` table in `config.toml`, and `:playlist rule x` in the
//! player, which records a `RuleSaved` fact (#323). They differ in what the
//! question is: a table's `where` is a [`Filter`], and a saved rule is the
//! composed `:view` it was made from — a [`crate::view::Stack`], so
//! `in="Jazz" &rating>=4` is something a rule can say. Both are [`Question`]s
//! here, and nothing past `members` can tell them apart.

use crate::model::rows::CopyId;
use crate::model::rows::Listing;
use crate::query::filter::Filter;
use crate::query::sort::{Sort, SortEnv};
use crate::view::Definitions;

/// Which tracks a rule takes — a predicate, or a composed view.
#[derive(Debug, Clone, PartialEq)]
pub enum Question {
    /// A `[playlists.x] where = "…"` table: one predicate over every copy.
    Where(Filter),
    /// A rule saved from the player: the stack `:view` had composed (#323).
    ///
    /// **Resolved as the view resolves it**, so a rule saved from what you
    /// were looking at holds what you were looking at — recordings, and every
    /// copy of each — rather than being re-read by a second set of semantics.
    View(crate::view::Stack),
}

impl Question {
    /// What was written — the line a person typed, either way.
    pub fn source(&self) -> String {
        match self {
            Question::Where(filter) => filter.source.clone(),
            Question::View(stack) => stack.label(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SmartPlaylist {
    pub name: String,
    /// Which tracks. Required — a smart playlist with no rule is the library.
    pub question: Question,
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
    ///
    /// `defs` is where a saved rule's `saved="…"` terms are looked up; a config
    /// rule names none and ignores it.
    pub fn members(
        &self,
        rows: &dyn Listing,
        all: &[CopyId],
        defs: &dyn Definitions,
        env: SortEnv<'_>,
    ) -> Vec<CopyId> {
        let mut ids: Vec<CopyId> = match &self.question {
            // **One row per recording, the best of the copies that matched** (#325), the
            // way a view's rule is answered. A rating matches every copy, so `rating>=4`
            // held Boadicea seven times.
            Question::Where(filter) => {
                let matches = filter.compile(env.env);
                let matched: Vec<CopyId> = all
                    .iter()
                    .copied()
                    // **The row is owned and the subject borrows it** — #261. `Listing::subject`
                    // used to hand back a borrow into the fold, which a reader on a socket has
                    // none of; a slip is the same row written out, and a `Subject` over it reads
                    // identically.
                    .filter(|id| {
                        rows.listed(*id).is_some_and(|slip| {
                            matches(&crate::model::rows::Subject {
                                row: slip.listed(),
                                locator: None,
                                recording: rows.recording_of(*id),
                            })
                        })
                    })
                    .collect();
                let held = crate::model::rows::best_of_each(rows, matched);
                all.iter().copied().filter(|id| held.contains(id)).collect()
            }
            Question::View(stack) => {
                let resolved = stack.resolve(rows, defs, env.env);
                // **A rule naming a saved view that has gone holds nothing**, and
                // [`Self::trouble`] is what says why — the empty list alone would read as
                // a query that matched nothing (#119).
                if !resolved.problems.is_empty() {
                    return Vec::new();
                }
                let held = resolved.copies(rows);
                all.iter().copied().filter(|id| held.contains(id)).collect()
            }
        };
        self.sort.apply(&mut ids, rows, env);
        ids
    }

    /// Why this rule cannot be answered, if it cannot.
    ///
    /// Only a saved rule can fail this way: it is a view, and a view may name
    /// a saved view that has since been dropped. A config `where` names none.
    pub fn trouble(
        &self,
        rows: &dyn Listing,
        defs: &dyn Definitions,
        env: SortEnv<'_>,
    ) -> Option<String> {
        let Question::View(stack) = &self.question else {
            return None;
        };
        let resolved = stack.resolve(rows, defs, env.env);
        resolved.problems.first().map(|p| p.to_string())
    }

    /// What the rule said, for a message and for view 3's title.
    pub fn rule_label(&self) -> String {
        format!(
            "{}  sorted by  {}",
            self.question.source(),
            self.sort.source
        )
    }

    /// A rule the log holds, read back through the parsers that wrote it — #323.
    ///
    /// **An error rather than a skip**, because the only way a stored line
    /// stops parsing is the language changing under it, and a rule that
    /// silently vanished from view 7 would be the worse answer.
    pub fn saved(rule: &crate::projection::catalogue::SavedRule) -> Result<Self, String> {
        let stack = crate::view::Stack::parse(&rule.source).map_err(|e| {
            format!(
                "{} was saved as {:?}, which no longer parses: {e}",
                rule.name, rule.source
            )
        })?;
        let sort = if rule.sort.is_empty() {
            Sort::library_order()
        } else {
            Sort::parse(&rule.sort).map_err(|e| {
                format!(
                    "{} was saved sorted by {:?}, which no longer parses: {e}",
                    rule.name, rule.sort
                )
            })?
        };
        Ok(SmartPlaylist {
            name: rule.name.clone(),
            question: Question::View(stack),
            sort,
        })
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
            question: Question::Where(filter),
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
    use crate::fixture::Tagged as Track;
    use crate::model::rows::Listing;
    use crate::operations::scan::Holdings as Scan;
    use crate::query::filter::Env;
    use crate::query::sort::Sort;
    use std::path::PathBuf;
    use std::time::Duration;

    /// A library of three files, folded the way a launch would.
    fn store() -> crate::model::seam::Librarian {
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
                // A year is a fact about a *pressing*, so a file needs an
                // album for one to have anywhere to live (#86).
                album: Some(format!("{artist} Album")),
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
        crate::fixture::from_holdings(&s)
    }

    /// The library's answers about listening, built by hand.
    ///
    /// A real `Catalogue` rather than a stub, because it is the thing the
    /// player actually asks and its fields are the fold's own output.
    fn listening() -> crate::projection::heard::Heard {
        crate::projection::heard::Heard::default()
    }

    /// Note a play against whatever recording `path` is, the way the log would.
    fn played(
        cat: &mut crate::projection::heard::Heard,
        l: &crate::model::seam::Librarian,
        path: &str,
        at: i64,
    ) {
        // **The seam takes no locator any more** (#120), so a test naming a
        // path finds the row that reports it rather than asking by location.
        let id = Listing::rows(l)
            .into_iter()
            .find(|id| Listing::playable(l, *id).is_some_and(|p| p.locator == path))
            .expect("a known track");
        let recording = Listing::recording_of(l, id).expect("with an identity");
        *cat.plays.entry(recording).or_insert(0) += 1;
        cat.last_played.insert(recording, at);
    }

    fn env(h: &crate::projection::heard::Heard) -> SortEnv<'_> {
        SortEnv {
            env: Env {
                listened: Some(h),
                named: None,
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
            crate::query::filter::Filter::parse(where_).unwrap(),
            sort.map(|s| Sort::parse(s).unwrap()),
        );
        set.get("p").unwrap().clone()
    }

    fn names(p: &SmartPlaylist, l: &crate::model::seam::Librarian, e: SortEnv<'_>) -> Vec<String> {
        p.members(l, &Listing::rows(l), &crate::view::NoDefinitions, e)
            .iter()
            .map(|id| {
                Listing::listed(l, *id)
                    .expect("a row")
                    .artist
                    .unwrap_or_default()
                    .to_string()
            })
            .collect()
    }

    /// **A config rule holds each recording once, by its best copy** — #325.
    /// A rating matches every copy of a recording, so `rating>=4` held Boadicea
    /// once per album, compilation and encoding.
    #[test]
    fn a_rule_holds_each_recording_once_by_its_best_copy() {
        let mut s = Scan::new();
        for (n, (path, album, compilation, bitrate)) in [
            ("/m/Enya/The Celts/Boadicea.mp3", "The Celts", false, 320),
            (
                "/m/Enya/Paint the Sky/Boadicea.flac",
                "Paint the Sky",
                true,
                900,
            ),
        ]
        .into_iter()
        .enumerate()
        {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some("Enya".into()),
                album: Some(album.into()),
                title: Some("Boadicea".into()),
                bitrate: Some(bitrate),
                duration: Duration::from_secs(210),
                mb_recording: Some(uuid::Uuid::from_u128(7)),
                release_secondary: if compilation {
                    [crate::model::spine::ReleaseSecondary::Compilation]
                        .into_iter()
                        .collect()
                } else {
                    Default::default()
                },
                audio_id: Some(crate::audio_id::AudioId {
                    scheme: 1,
                    hash: n as u128 + 1,
                }),
                ..Default::default()
            });
        }
        let l = crate::fixture::from_holdings(&s);
        let all = Listing::rows(&l);
        // The instrument: two rows, and both of one recording.
        assert_eq!(all.len(), 2);
        assert_eq!(
            Listing::recording_of(&l, all[0]),
            Listing::recording_of(&l, all[1])
        );

        let h = listening();
        let members = playlist(r#"artist="Enya""#, None).members(
            &l,
            &all,
            &crate::view::NoDefinitions,
            env(&h),
        );
        let albums: Vec<String> = members
            .iter()
            .map(|id| Listing::listed(&l, *id).expect("a row").album)
            .collect();
        assert_eq!(albums, ["Paint the Sky"], "the FLAC is the better copy");
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
            .members(&s, &Listing::rows(&s), &crate::view::NoDefinitions, env(&h))
            .iter()
            .map(|id| {
                Listing::playable(&s, *id)
                    .expect("held here")
                    .locator
                    .to_string()
            })
            .collect();
        assert_eq!(paths, ["/m/General/b.mp3", "/m/Dance/c.mp3"]);
    }

    #[test]
    fn a_rule_that_matches_nothing_is_empty_rather_than_everything() {
        let (s, h) = (store(), listening());
        let p = playlist("genre=\"polka\"", None);
        assert!(
            p.members(&s, &Listing::rows(&s), &crate::view::NoDefinitions, env(&h))
                .is_empty()
        );
    }

    /// The rule is re-read on every call, so a play or a rating changes the
    /// answer without anything having to invalidate a cached list.
    #[test]
    fn history_rules_answer_differently_as_history_changes() {
        let s = store();
        let mut h = listening();
        let p = playlist("play_count>0", None);
        assert!(
            p.members(&s, &Listing::rows(&s), &crate::view::NoDefinitions, env(&h))
                .is_empty()
        );

        played(&mut h, &s, "/m/General/b.mp3", 900_000);
        assert_eq!(names(&p, &s, env(&h)), ["Burial"]);
    }

    #[test]
    fn defining_the_same_name_twice_replaces_it() {
        let mut set = SmartPlaylists::default();
        set.define(
            "p",
            crate::query::filter::Filter::parse("year>0").unwrap(),
            None,
        );
        set.define(
            "p",
            crate::query::filter::Filter::parse("year>2000").unwrap(),
            None,
        );
        assert_eq!(set.names(), ["p"]);
        assert_eq!(set.get("p").unwrap().question.source(), "year>2000");
    }

    fn saved(source: &str, sort: &str) -> crate::projection::catalogue::SavedRule {
        crate::projection::catalogue::SavedRule {
            name: "r".into(),
            source: source.into(),
            sort: sort.into(),
        }
    }

    /// **A saved rule is a view**, so it reads `in=` and `saved=` terms a
    /// config `where` cannot — and naming a saved view that has gone is said,
    /// rather than reading as a query that matched nothing (#119, #323).
    #[test]
    fn a_saved_rule_naming_a_view_that_is_gone_says_so() {
        let (s, h) = (store(), listening());
        // **A union**, so the half that does resolve has rows to give: without the
        // guard this would answer with the electronic tracks and say nothing.
        let rule = SmartPlaylist::saved(&saved(r#"genre="electronic" +saved="gone""#, ""))
            .expect("it parses");
        assert!(
            rule.members(&s, &Listing::rows(&s), &crate::view::NoDefinitions, env(&h))
                .is_empty()
        );
        let why = rule.trouble(&s, &crate::view::NoDefinitions, env(&h));
        assert!(
            why.as_deref().is_some_and(|w| w.contains("gone")),
            "{why:?}"
        );

        // A rule that resolves has nothing to report, so the line above is
        // about the missing view and not about every saved rule.
        let fine = SmartPlaylist::saved(&saved(r#"genre="electronic""#, "-year")).unwrap();
        assert_eq!(fine.trouble(&s, &crate::view::NoDefinitions, env(&h)), None);
        assert_eq!(names(&fine, &s, env(&h)), ["Caribou", "Burial"]);
    }

    /// A stored line that no longer parses is an error naming the rule, not a
    /// rule that quietly vanished.
    #[test]
    fn a_saved_rule_that_no_longer_parses_says_which() {
        let bad_sort = SmartPlaylist::saved(&saved("year>0", "-nonsense_field"));
        assert!(
            bad_sort.as_ref().is_err_and(|e| e.contains("sorted by")),
            "{bad_sort:?}"
        );
        let bad_view = SmartPlaylist::saved(&saved("year>>0", ""));
        assert!(
            bad_view
                .as_ref()
                .is_err_and(|e| e.contains("no longer parses")),
            "{bad_view:?}"
        );
    }

    #[test]
    fn the_label_names_both_halves() {
        let p = playlist("rating>=4", Some("-rating, artist"));
        assert_eq!(p.rule_label(), "rating>=4  sorted by  -rating, artist");
    }
}
