//! The sort language — DESIGN §7, issue #15.
//!
//! A comma-separated list of field names, applied left to right, with `-` for
//! descending:
//!
//! ```text
//! artist, album, disc, track     the natural album order
//! -rating, -play_count           best and most played first
//! random
//! ```
//!
//! The keys are the field names the format and filter languages already use.
//! That is the whole design: **one vocabulary, now for a third purpose**, so
//! adding a field to [`crate::format::Field`] gives all three of them.
//!
//! foobar2000 instead sorts by an arbitrary title-format pattern
//! (`SORT BY %album%|$num(%tracknumber%,2)`), which is more general and makes
//! numbers sort lexically unless you remember to pad them by hand. iTunes
//! offers a fixed menu with no composition at all. A named vocabulary with
//! type-aware comparison is the middle these two miss.

use std::cmp::Ordering;

use crate::filter::Env;
use crate::format::{Field, Kind};
use crate::library::sort_key;
use crate::model::entity::Recording;
use crate::model::id::Id;
use crate::store::{Store, Track, TrackId};

/// What one sort key looks at.
///
/// The history keys are spelled as **timestamps** here — `added`, `last_played`
/// — while the filter language spells the same data as `days_since_added` and
/// `days_since_played`. That is deliberate, and it is the one place these two
/// languages disagree.
///
/// `days_since_added<30` reads in a filter. In a sort it inverts: *ascending*
/// would mean newest first, because fewer days since means more recent. Writing
/// `-days_since_added` expecting the newest gives the oldest, **silently**, and
/// the resulting list still looks plausible. `-added` cannot be misread.
///
/// Each language therefore has exactly one spelling, and each rejects the
/// other's by name — see [`SortError::WrongLanguage`], which says what to write
/// instead rather than "unknown field".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// A field from the shared vocabulary.
    Field(Field),
    PlayCount,
    Rating,
    /// When gmuse first saw the file. `-added` is newest first.
    Added,
    /// When it was last played. `-last_played` is most recently played first.
    LastPlayed,
    /// A different order every time the seed changes.
    Random,
}

impl Key {
    /// Every name, for error messages and completion.
    pub fn names() -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Field::NAMES.to_vec();
        names.extend_from_slice(&["added", "last_played", "play_count", "random", "rating"]);
        names.sort_unstable();
        names
    }

    fn parse(name: &str) -> Result<Self, SortError> {
        if let Some(f) = Field::parse(name) {
            return Ok(Key::Field(f));
        }
        Ok(match name {
            "play_count" => Key::PlayCount,
            "rating" => Key::Rating,
            "added" => Key::Added,
            "last_played" => Key::LastPlayed,
            "random" => Key::Random,
            // The filter language's spelling of the two date fields. Naming the
            // replacement costs one message; guessing costs a silently
            // reversed list.
            "days_since_added" => {
                return Err(SortError::WrongLanguage {
                    name: name.to_string(),
                    instead: "added",
                });
            }
            "days_since_played" => {
                return Err(SortError::WrongLanguage {
                    name: name.to_string(),
                    instead: "last_played",
                });
            }
            _ => {
                return Err(SortError::UnknownKey {
                    name: name.to_string(),
                });
            }
        })
    }
}

/// One key and its direction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Term {
    pub key: Key,
    pub descending: bool,
}

/// A parsed sort order, and the text it was written as.
///
/// The source is kept for the same reason [`crate::filter::Filter`] keeps its
/// own: §6 wants a command to round-trip through text, and rendering the terms
/// back would normalise away the spacing someone chose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    pub source: String,
    pub terms: Vec<Term>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SortError {
    #[error("unknown sort key {name:?} — try one of: {}", Key::names().join(", "))]
    UnknownKey { name: String },
    #[error("{name} is how a filter spells it — in a sort, use `{instead}`")]
    WrongLanguage { name: String, instead: &'static str },
    #[error("a sort key is empty — two commas with nothing between them")]
    EmptyTerm,
    #[error("the sort is empty")]
    Empty,
}

/// What sorting needs beyond the tracks themselves.
#[derive(Debug, Clone, Copy)]
pub struct SortEnv<'a> {
    /// History and the clock, shared with the filter language.
    pub env: Env<'a>,
    /// Honoured by the artist keys, so a playlist sorted by artist agrees with
    /// the library tree about where "The Beatles" goes.
    pub smart_artist_sort: bool,
    /// Advanced by the caller to reshuffle a `random` sort.
    pub seed: u64,
}

impl Sort {
    /// What an unsorted smart playlist gets: the library's own order.
    ///
    /// By `file_under` rather than `artist`, for the same reason §11.4 files
    /// the tree that way — sorting a compilation by `artist` scatters it.
    pub fn library_order() -> Self {
        Sort::parse("file_under, album, disc, track").expect("a literal sort")
    }

    pub fn parse(source: &str) -> Result<Self, SortError> {
        let trimmed = source.trim();
        if trimmed.is_empty() {
            return Err(SortError::Empty);
        }
        let mut terms = Vec::new();
        for part in trimmed.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(SortError::EmptyTerm);
            }
            // `-` is the only prefix. `!` would collide with the filter
            // language's negation, which means something else entirely.
            let (descending, name) = match part.strip_prefix('-') {
                Some(rest) => (true, rest.trim()),
                None => (false, part),
            };
            if name.is_empty() {
                return Err(SortError::EmptyTerm);
            }
            terms.push(Term {
                key: Key::parse(&name.to_ascii_lowercase())?,
                descending,
            });
        }
        Ok(Sort {
            source: trimmed.to_string(),
            terms,
        })
    }

    /// Order `ids` in place.
    ///
    /// Ties are broken by path, so the result is a total order and does not
    /// wobble between runs — `sort = "artist"` over an album has to give the
    /// same list twice.
    pub fn apply(&self, ids: &mut [TrackId], store: &Store, env: SortEnv<'_>) {
        // Decorate-sort-undecorate: the comparator runs O(n log n) times and
        // building a lowercase key inside it would allocate on every one.
        let mut decorated: Vec<(Vec<Cell>, &std::path::Path, TrackId)> = ids
            .iter()
            .map(|id| {
                let track = store.get(*id);
                let cells = self
                    .terms
                    .iter()
                    .map(|t| match track {
                        Some(tr) => cell(t.key, *id, tr, &env),
                        None => Cell::Missing,
                    })
                    .collect();
                let path: &std::path::Path = match track {
                    Some(tr) => &tr.path,
                    None => std::path::Path::new(""),
                };
                (cells, path, *id)
            })
            .collect();

        decorated.sort_by(|a, b| {
            for (i, term) in self.terms.iter().enumerate() {
                let ord = compare(&a.0[i], &b.0[i], term.descending);
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            a.1.cmp(b.1)
        });

        for (slot, (_, _, id)) in ids.iter_mut().zip(decorated) {
            *slot = id;
        }
    }
}

/// One track's value for one key.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Cell {
    Text(String),
    Num(i64),
    /// No value: an untagged field, or a track never played.
    Missing,
}

/// Compare two cells, honouring the direction — **except for missing values,
/// which sort last either way.**
///
/// That makes the order not strictly reversible, which is a real wart and the
/// lesser one: the alternative opens `sort = "rating"` with a block of unrated
/// tracks, and a list whose first screen is blanks looks broken rather than
/// sorted.
fn compare(a: &Cell, b: &Cell, descending: bool) -> Ordering {
    match (a, b) {
        (Cell::Missing, Cell::Missing) => Ordering::Equal,
        (Cell::Missing, _) => Ordering::Greater,
        (_, Cell::Missing) => Ordering::Less,
        (Cell::Text(x), Cell::Text(y)) => flip(x.cmp(y), descending),
        (Cell::Num(x), Cell::Num(y)) => flip(x.cmp(y), descending),
        // Two cells for the same key are always the same shape.
        _ => Ordering::Equal,
    }
}

fn flip(ord: Ordering, descending: bool) -> Ordering {
    if descending { ord.reverse() } else { ord }
}

/// The history source and the identity to ask it about.
///
/// Both or neither: without an identity there is no question to ask, which is
/// what #81 replaces "look it up by path, and hope it has not moved" with.
fn listened_of<'a>(
    t: &Track,
    env: &SortEnv<'a>,
) -> Option<(&'a dyn crate::filter::Listened, Id<Recording>)> {
    let listened = env.env.listened?;
    let recording = crate::history::recording_id_for(t.mb_recording, t.audio_id)?;
    Some((listened, recording))
}

fn cell(key: Key, id: TrackId, t: &Track, env: &SortEnv<'_>) -> Cell {
    match key {
        Key::Field(f) => match f.kind() {
            // Case-folded, and with a leading article moved aside for the
            // artist keys — the same normalisation the library tree uses, so
            // the two agree about where a name belongs.
            Kind::Text => match crate::filter::text_of(f, t) {
                Some(v) if !v.trim().is_empty() => {
                    let smart = env.smart_artist_sort && is_artist(f);
                    Cell::Text(sort_key(&v, smart).as_str().to_string())
                }
                _ => Cell::Missing,
            },
            Kind::Number => match crate::filter::number_of(f, t) {
                Some(v) => Cell::Num(v),
                None => Cell::Missing,
            },
        },
        // Never played is zero plays, not "cannot say" — the same answer the
        // filter language gives `play_count=0`.
        Key::PlayCount => Cell::Num(i64::from(
            listened_of(t, env).map(|(l, r)| l.plays(r)).unwrap_or(0),
        )),
        Key::Rating => match listened_of(t, env).and_then(|(l, r)| l.rating(r)) {
            Some(r) => Cell::Num(i64::from(r)),
            None => Cell::Missing,
        },
        Key::Added => match listened_of(t, env).and_then(|(l, r)| l.acquired(r)) {
            Some(at) => Cell::Num(at),
            None => Cell::Missing,
        },
        Key::LastPlayed => match listened_of(t, env).and_then(|(l, r)| l.last_played(r)) {
            Some(v) => Cell::Num(v),
            None => Cell::Missing,
        },
        // Hashed rather than drawn, so the comparator stays a valid ordering:
        // a key that answered differently each time it was asked would make
        // `sort_by` produce garbage rather than a shuffle.
        Key::Random => Cell::Num(mix(u64::from(id.index()), env.seed) as i64),
    }
}

fn is_artist(f: Field) -> bool {
    matches!(f, Field::Artist | Field::AlbumArtist | Field::FileUnder)
}

/// SplitMix64 over the id and the seed. `next.rs` uses the same generator to
/// shuffle a pool; this is the stateless form of it.
fn mix(id: u64, seed: u64) -> u64 {
    let mut z = id.wrapping_add(seed).wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::log::Catalogue;
    use std::path::PathBuf;
    use std::time::Duration;

    fn store() -> Store {
        let mut s = Store::new();
        // Track numbers either side of ten: the case foobar2000's
        // format-pattern sorting gets wrong without `$num()` padding.
        for (n, (path, artist, title, track)) in [
            ("/m/b.mp3", "The Beatles", "Beta", 10u32),
            ("/m/a.mp3", "abba", "Alpha", 2),
            ("/m/c.mp3", "Zappa", "Gamma", 1),
        ]
        .into_iter()
        .enumerate()
        {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some(artist.into()),
                title: Some(title.into()),
                tracknumber: Some(track),
                duration: Duration::from_secs(100),
                // History hangs off an identity now, so a sortable track
                // needs one. The orphan below deliberately has none.
                audio_id: Some(crate::audio_id::AudioId {
                    scheme: 1,
                    hash: n as u128 + 1,
                }),
                ..Default::default()
            });
        }
        // Untagged, so every key is missing for it.
        s.upsert(Track {
            path: PathBuf::from("/m/z_orphan.mp3"),
            duration: Duration::from_secs(100),
            ..Default::default()
        });
        s
    }

    fn ids(s: &Store) -> Vec<TrackId> {
        s.iter().map(|(id, _)| id).collect()
    }

    /// The recording a test path is, so a fact can name it.
    fn rec_of(s: &Store, path: &str) -> Id<Recording> {
        let id = s.id_for_path(&PathBuf::from(path)).expect("a known track");
        s.recording_of(id).expect("with an identity")
    }

    fn acquired(cat: &mut Catalogue, s: &Store, path: &str, at: i64) {
        cat.acquired.insert(rec_of(s, path), at);
    }

    fn played(cat: &mut Catalogue, s: &Store, path: &str, at: i64) {
        let r = rec_of(s, path);
        *cat.plays.entry(r).or_insert(0) += 1;
        cat.last_played.insert(r, at);
    }

    fn rated(cat: &mut Catalogue, s: &Store, path: &str, stars: u8) {
        cat.ratings.insert(rec_of(s, path), stars);
    }

    fn env(listened: &Catalogue) -> SortEnv<'_> {
        SortEnv {
            env: Env {
                listened: Some(listened),
                now: 1_000_000,
            },
            smart_artist_sort: false,
            seed: 7,
        }
    }

    /// The order as file names, which is short enough to assert on directly.
    fn order(sort: &str, s: &Store, e: SortEnv<'_>) -> Vec<String> {
        let mut v = ids(s);
        Sort::parse(sort).expect("parses").apply(&mut v, s, e);
        v.iter()
            .map(|id| {
                s.get(*id)
                    .unwrap()
                    .path
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }

    #[test]
    fn parses_terms_left_to_right_with_a_minus_for_descending() {
        let s = Sort::parse(" artist , -year,track ").unwrap();
        assert_eq!(
            s.terms,
            vec![
                Term {
                    key: Key::Field(Field::Artist),
                    descending: false
                },
                Term {
                    key: Key::Field(Field::Year),
                    descending: true
                },
                Term {
                    key: Key::Field(Field::Track),
                    descending: false
                },
            ]
        );
        // The source is kept verbatim, spacing and all.
        assert_eq!(s.source, "artist , -year,track");
    }

    #[test]
    fn the_history_keys_are_timestamps_here() {
        assert_eq!(Sort::parse("-added").unwrap().terms[0].key, Key::Added);
        assert_eq!(
            Sort::parse("last_played").unwrap().terms[0].key,
            Key::LastPlayed
        );
    }

    /// The whole point of spelling dates differently in the two languages: a
    /// wrong guess has to cost a message, not a silently reversed list.
    #[test]
    fn a_filters_date_spelling_is_refused_by_name() {
        let e = Sort::parse("-days_since_added").unwrap_err();
        assert_eq!(
            e,
            SortError::WrongLanguage {
                name: "days_since_added".into(),
                instead: "added",
            }
        );
        assert!(e.to_string().contains("use `added`"), "{e}");

        let e = Sort::parse("days_since_played").unwrap_err();
        assert!(e.to_string().contains("use `last_played`"), "{e}");
    }

    /// And the reverse, so neither language leaves you guessing.
    #[test]
    fn a_sorts_date_spelling_is_refused_by_the_filter() {
        let e = crate::filter::Filter::parse("added>3").unwrap_err();
        assert!(e.to_string().contains("use `days_since_added`"), "{e}");
        let e = crate::filter::Filter::parse("last_played>3").unwrap_err();
        assert!(e.to_string().contains("use `days_since_played`"), "{e}");
    }

    #[test]
    fn errors_are_specific() {
        assert_eq!(
            Sort::parse("frobnicate").unwrap_err(),
            SortError::UnknownKey {
                name: "frobnicate".into()
            }
        );
        assert_eq!(
            Sort::parse("artist,,year").unwrap_err(),
            SortError::EmptyTerm
        );
        assert_eq!(Sort::parse("artist,-").unwrap_err(), SortError::EmptyTerm);
        assert_eq!(Sort::parse("   ").unwrap_err(), SortError::Empty);
        // The message lists what would have worked.
        let msg = Sort::parse("nope").unwrap_err().to_string();
        assert!(msg.contains("random"), "{msg}");
        assert!(msg.contains("album"), "{msg}");
    }

    #[test]
    fn text_sorts_case_folded() {
        let (s, h) = (store(), Catalogue::default());
        // "abba" before "The Beatles" before "Zappa" — and lower-case `abba`
        // must not sort after every capital, which a byte comparison would do.
        assert_eq!(
            order("artist", &s, env(&h))[..3],
            ["a.mp3", "b.mp3", "c.mp3"]
        );
    }

    #[test]
    fn artist_keys_honour_smart_artist_sort() {
        // Chosen so the article actually decides the order: "The Zombies"
        // files under Z with the setting on and under T with it off, which
        // puts it either side of "Yes".
        let mut s = Store::new();
        for (path, artist) in [("/m/z.mp3", "The Zombies"), ("/m/y.mp3", "Yes")] {
            s.upsert(Track {
                path: PathBuf::from(path),
                artist: Some(artist.into()),
                ..Default::default()
            });
        }
        let h = Catalogue::default();

        let mut e = env(&h);
        e.smart_artist_sort = true;
        assert_eq!(order("artist", &s, e), ["y.mp3", "z.mp3"]);

        // Off, "The Zombies" sorts under T and comes first.
        assert_eq!(order("artist", &s, env(&h)), ["z.mp3", "y.mp3"]);
    }

    /// The setting applies to *artists* only — "The Wall" is an album whose T
    /// matters, which is the rule `library::sort_key` already encodes.
    #[test]
    fn smart_artist_sort_does_not_touch_album_titles() {
        let mut s = Store::new();
        for (path, album) in [("/m/w.mp3", "The Wall"), ("/m/u.mp3", "Ummagumma")] {
            s.upsert(Track {
                path: PathBuf::from(path),
                album: Some(album.into()),
                ..Default::default()
            });
        }
        let h = Catalogue::default();
        let mut e = env(&h);
        e.smart_artist_sort = true;
        // Under T, so before Ummagumma. Stripping the article would put
        // "wall" after it, which is how this test fails if the rule leaks.
        assert_eq!(order("album", &s, e), ["w.mp3", "u.mp3"]);
    }

    /// Numbers compare as numbers. Sorting them as rendered text — foobar's
    /// model without `$num()` — would put track 10 before track 2.
    #[test]
    fn numbers_do_not_sort_lexically() {
        let (s, h) = (store(), Catalogue::default());
        assert_eq!(
            order("track", &s, env(&h))[..3],
            ["c.mp3", "a.mp3", "b.mp3"]
        );
    }

    /// Missing last in *both* directions, so a list never opens with blanks.
    #[test]
    fn a_missing_value_sorts_last_whichever_way_the_key_points() {
        let (s, h) = (store(), Catalogue::default());
        assert_eq!(order("track", &s, env(&h))[3], "z_orphan.mp3");
        assert_eq!(order("-track", &s, env(&h))[3], "z_orphan.mp3");
        assert_eq!(order("artist", &s, env(&h))[3], "z_orphan.mp3");
        assert_eq!(order("-artist", &s, env(&h))[3], "z_orphan.mp3");
    }

    #[test]
    fn descending_reverses_the_present_values() {
        let (s, h) = (store(), Catalogue::default());
        assert_eq!(
            order("-track", &s, env(&h))[..3],
            ["b.mp3", "a.mp3", "c.mp3"]
        );
    }

    #[test]
    fn later_terms_break_earlier_ties() {
        let mut s = Store::new();
        for (path, album, track) in [
            ("/m/1.mp3", "B", 2u32),
            ("/m/2.mp3", "A", 9),
            ("/m/3.mp3", "B", 1),
        ] {
            s.upsert(Track {
                path: PathBuf::from(path),
                album: Some(album.into()),
                tracknumber: Some(track),
                ..Default::default()
            });
        }
        let h = Catalogue::default();
        assert_eq!(
            order("album, track", &s, env(&h)),
            ["2.mp3", "3.mp3", "1.mp3"]
        );
    }

    /// Ties are broken by path, so the same input gives the same output —
    /// otherwise `sort = "album"` over one album would wobble between runs.
    #[test]
    fn a_total_tie_is_still_deterministic() {
        let mut s = Store::new();
        for path in ["/m/c.mp3", "/m/a.mp3", "/m/b.mp3"] {
            s.upsert(Track {
                path: PathBuf::from(path),
                album: Some("One".into()),
                ..Default::default()
            });
        }
        let h = Catalogue::default();
        assert_eq!(order("album", &s, env(&h)), ["a.mp3", "b.mp3", "c.mp3"]);
    }

    #[test]
    fn random_is_a_permutation_that_follows_the_seed() {
        let (s, h) = (store(), Catalogue::default());
        let a = order("random", &s, env(&h));
        let b = order("random", &s, env(&h));
        assert_eq!(a, b, "the same seed must give the same order");

        let mut e = env(&h);
        e.seed = 99;
        let c = order("random", &s, e);
        assert_ne!(a, c, "a new seed must give a new order");

        // Still every track, exactly once.
        let mut sorted = c.clone();
        sorted.sort();
        let mut all = a.clone();
        all.sort();
        assert_eq!(sorted, all);
    }

    #[test]
    fn history_keys_read_the_history() {
        let s = store();
        let mut h = Catalogue::default();
        acquired(&mut h, &s, "/m/a.mp3", 100);
        acquired(&mut h, &s, "/m/b.mp3", 200);
        played(&mut h, &s, "/m/b.mp3", 500);
        rated(&mut h, &s, "/m/a.mp3", 5);

        // Newest first.
        assert_eq!(order("-added", &s, env(&h))[..2], ["b.mp3", "a.mp3"]);
        // Oldest first, and the two with no entry at all come last.
        assert_eq!(order("added", &s, env(&h))[..2], ["a.mp3", "b.mp3"]);
        // Only b has ever been played; the rest are missing, so they trail.
        assert_eq!(order("-last_played", &s, env(&h))[0], "b.mp3");
        // Rated first, unrated last — not "nought stars first".
        assert_eq!(order("-rating", &s, env(&h))[0], "a.mp3");
        assert_eq!(order("rating", &s, env(&h))[0], "a.mp3");
    }

    /// Never played is zero plays, matching what `play_count=0` means in a
    /// filter — so it sorts as a number rather than trailing as missing.
    #[test]
    fn play_count_is_zero_rather_than_missing() {
        let s = store();
        let mut h = Catalogue::default();
        acquired(&mut h, &s, "/m/a.mp3", 100);
        played(&mut h, &s, "/m/a.mp3", 500);
        // The unplayed ones sort first ascending, which they could not do if
        // they were missing.
        assert_eq!(order("play_count", &s, env(&h))[3], "a.mp3");
        assert_eq!(order("-play_count", &s, env(&h))[0], "a.mp3");
    }

    #[test]
    fn the_default_order_is_the_librarys() {
        assert_eq!(
            Sort::library_order().source,
            "file_under, album, disc, track"
        );
    }
}
