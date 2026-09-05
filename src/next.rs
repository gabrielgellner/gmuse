//! Next-track resolution — DESIGN §5 (INVARIANT).
//!
//! This is what makes the player feel correct, and it is where every "why did
//! it play *that*" bug lives. It is a pure function over explicit state: no
//! I/O, no mutation, and no reaching into the player. Everything it needs is
//! in [`NextInput`], and everything it decides comes back as a value.
//!
//! Two consequences of that purity worth knowing before reading on:
//!
//! - **It does not pop the queue.** It reports that the queue *would* supply
//!   the next track ([`Source::Queue`]); the caller pops once it commits.
//! - **Shuffle is a seeded permutation, not a played-set.** See [`shuffled`].

use crate::model::entity::Encoding;
use crate::model::id::Id;
use crate::playlist::{Playlist, Queue};
use crate::shelf::Shelf;
use crate::store::TrackId;

/// Which list the player is walking through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActiveSource {
    #[default]
    Library,
    Playlist,
}

/// cmus's "all / artist / album" narrowing. Applies to the library only.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AaaMode {
    #[default]
    All,
    Artist,
    Album,
}

impl AaaMode {
    pub fn cycle(self) -> Self {
        match self {
            AaaMode::All => AaaMode::Artist,
            AaaMode::Artist => AaaMode::Album,
            AaaMode::Album => AaaMode::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AaaMode::All => "all",
            AaaMode::Artist => "artist",
            AaaMode::Album => "album",
        }
    }

    /// The inverse of [`AaaMode::label`], for reading a saved session.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all" => AaaMode::All,
            "artist" => AaaMode::Artist,
            "album" => AaaMode::Album,
            _ => return None,
        })
    }
}

/// Where the chosen track came from, so the caller knows what to do next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `repeat_current` held the track in place. Nothing to consume.
    RepeatCurrent,
    /// The head of the queue. **The caller must pop it.**
    Queue,
    /// The successor within the candidate pool.
    Pool,
    /// The pool ran out and `repeat` wrapped it. When shuffling, the caller
    /// should advance `rng_state` so the next lap is a different order.
    Wrapped,
}

/// Everything a decision depends on.
pub struct NextInput<'a> {
    pub queue: &'a Queue,
    pub active: ActiveSource,
    pub library: &'a Shelf,
    pub playlist: &'a Playlist,
    pub current: Option<TrackId>,
    pub aaa_mode: AaaMode,
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_current: bool,
    pub rng_state: u64,
}

/// The next track to play, or `None` to stop.
///
/// This is the signature DESIGN §5 specifies. The player itself calls
/// [`next_with_source`], because it needs to know whether to pop the queue.
#[allow(dead_code)]
pub fn next(input: NextInput<'_>) -> Option<TrackId> {
    next_with_source(input).map(|(id, _)| id)
}

/// As [`next`], but says where the track came from.
///
/// Resolution order is §5's, exactly:
///
/// 1. `repeat_current` holds the current track.
/// 2. **The queue wins unconditionally** — regardless of `aaa_mode`, shuffle,
///    or which source is active. Playing a queued track does not move the
///    library cursor, which is why `current` is left alone here.
/// 3. Otherwise, the successor within the pool.
/// 4. Pool exhausted: wrap if `repeat`, else stop.
pub fn next_with_source(input: NextInput<'_>) -> Option<(TrackId, Source)> {
    if input.repeat_current
        && let Some(current) = input.current
    {
        return Some((current, Source::RepeatCurrent));
    }
    if let Some(queued) = input.queue.front() {
        return Some((queued, Source::Queue));
    }
    step(&input, Direction::Forward)
}

/// Where a projected track comes from, and therefore whether it can be moved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upcoming {
    /// From the queue: a real list, so this row is reorderable.
    Queued,
    /// From the pool's order. A projection, not a promise — see [`upcoming`].
    Pool,
    /// The first track after `repeat` wraps the pool. The order **after** this
    /// point is not the order shown: `Source::Wrapped` advances `rng_state`, so
    /// a shuffled second lap is a different permutation.
    Wraps,
}

/// What would play next, in order, if nothing else were done.
///
/// **This costs nothing new**, and that is the point. Shuffle is a seeded
/// permutation (see [`shuffled`]), so the play order is already a pure function
/// of the pool and the seed — `step` materialises the whole thing on every call
/// and keeps one element of it. Projecting the future is reading the rest of
/// the list that was computed anyway, not simulating forward.
///
/// **A projection, not a promise.** The tail is recomputed whenever the pool
/// moves — a `:filter`, a catalog switch, a rating that changes a smart
/// playlist's membership. That follows from §7's "you hear what you can see"
/// and is correct; it is also why a caller should draw this as a *projection*
/// rather than as a queue that has been promised.
///
/// Stops at `limit`, because `repeat` makes the real answer infinite.
pub fn upcoming(input: &NextInput<'_>, limit: usize) -> Vec<(TrackId, Upcoming)> {
    let mut out = Vec::with_capacity(limit);
    if input.repeat_current {
        // Nothing else is coming: the current track holds until the setting
        // changes. Saying so is more useful than listing a future that will
        // not happen.
        return out;
    }
    out.extend(
        input
            .queue
            .entries()
            .take(limit)
            .map(|id| (*id, Upcoming::Queued)),
    );
    if out.len() >= limit {
        return out;
    }

    let pool = pool(input);
    if pool.is_empty() {
        return out;
    }
    let order = if input.shuffle {
        shuffled(&pool, input.rng_state)
    } else {
        pool
    };
    // Where the pool's own successor would start, by exactly the rule `step`
    // uses — including its fallback when the current track is not in the pool
    // at all, which happens after a queued track or an `aaa_mode` change.
    let start = input
        .current
        .and_then(|c| order.iter().position(|id| *id == c))
        .map_or(0, |p| p + 1);

    for id in order.iter().skip(start) {
        if out.len() >= limit {
            return out;
        }
        out.push((*id, Upcoming::Pool));
    }
    if input.repeat && out.len() < limit {
        // One row past the wrap, and no further: after this the seed advances
        // and the order shown would be wrong.
        if let Some(first) = order.first() {
            out.push((*first, Upcoming::Wraps));
        }
    }
    out
}

/// The previous track, or `None` at the start of the pool.
///
/// The queue is deliberately ignored: it is consumed as it plays, and there is
/// nothing sensible to un-pop.
pub fn prev(input: NextInput<'_>) -> Option<TrackId> {
    if input.repeat_current
        && let Some(current) = input.current
    {
        return Some(current);
    }
    step(&input, Direction::Backward).map(|(id, _)| id)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Direction {
    Forward,
    Backward,
}

fn step(input: &NextInput<'_>, dir: Direction) -> Option<(TrackId, Source)> {
    let pool = pool(input);
    if pool.is_empty() {
        return None;
    }

    let order = if input.shuffle {
        shuffled(&pool, input.rng_state)
    } else {
        pool
    };

    // Nothing playing: start at whichever end we are walking from.
    let Some(current) = input.current else {
        return Some(match dir {
            Direction::Forward => (order[0], Source::Pool),
            Direction::Backward => (order[order.len() - 1], Source::Pool),
        });
    };

    // The current track may not be in the pool at all — it was queued, or
    // `aaa_mode` changed under it. Starting from the top is the least
    // surprising thing to do.
    let Some(position) = order.iter().position(|id| *id == current) else {
        return Some(match dir {
            Direction::Forward => (order[0], Source::Pool),
            Direction::Backward => (order[order.len() - 1], Source::Pool),
        });
    };

    match dir {
        Direction::Forward => {
            if position + 1 < order.len() {
                Some((order[position + 1], Source::Pool))
            } else if input.repeat {
                Some((order[0], Source::Wrapped))
            } else {
                None
            }
        }
        Direction::Backward => {
            if position > 0 {
                Some((order[position - 1], Source::Pool))
            } else if input.repeat {
                Some((order[order.len() - 1], Source::Wrapped))
            } else {
                None
            }
        }
    }
}

/// The candidate pool: the active source, narrowed by `aaa_mode` when that
/// source is the library.
fn pool(input: &NextInput<'_>) -> Vec<TrackId> {
    match input.active {
        ActiveSource::Playlist => input.playlist.entries().to_vec(),
        ActiveSource::Library => match input.aaa_mode {
            AaaMode::All => input.library.sorted().to_vec(),
            AaaMode::Artist | AaaMode::Album => {
                // Narrowing is relative to what is playing. With nothing
                // playing there is no artist to narrow to, so fall back to the
                // whole library rather than returning nothing at all.
                let Some(current) = input.current else {
                    return input.library.sorted().to_vec();
                };
                let Some((artist, album)) = input.library.locate(current) else {
                    return input.library.sorted().to_vec();
                };
                match input.aaa_mode {
                    AaaMode::Artist => input.library.artist_tracks(artist),
                    _ => input.library.album_tracks(artist, album).to_vec(),
                }
            }
        },
    }
}

/// A deterministic permutation of the pool.
///
/// DESIGN §5 describes shuffle as "pick unplayed-at-random, tracking a
/// played-set per pool". A seeded permutation gives the same guarantee —
/// nothing repeats until the pool is exhausted — while keeping `next()` a pure
/// function, which §5 also demands. A played-set would have to be threaded in
/// and out on every call, and would have to be invalidated whenever the pool
/// changed; a permutation simply *is* the play order.
///
/// The cost is that repeating a shuffled pool replays the same order unless
/// the caller advances `rng_state`, which is what [`Source::Wrapped`] is for.
fn shuffled(pool: &[TrackId], seed: u64) -> Vec<TrackId> {
    let mut order = pool.to_vec();
    let mut state = seed ^ 0x9E37_79B9_7F4A_7C15;
    // Fisher-Yates, so every permutation is equally likely and no track can
    // appear twice.
    for i in (1..order.len()).rev() {
        state = split_mix64(state);
        let j = (state % (i as u64 + 1)) as usize;
        order.swap(i, j);
    }
    order
}

/// SplitMix64: small, fast, and good enough to shuffle a playlist. Avoids
/// taking a dependency on `rand` for one use.
fn split_mix64(state: u64) -> u64 {
    let mut z = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Extra state the caller keeps for the shuffle order to stay stable.
#[derive(Debug, Clone, Copy, Default)]
pub struct Playback {
    pub current: Option<TrackId>,
    /// **Which copy is being heard** — issue #85.
    ///
    /// Set when the library chooses one to open, so a play names the copy that
    /// actually played rather than the canonical one. Those are the same until
    /// a pin or a device's reach makes them differ, and the whole point of
    /// recording a play against an encoding is to answer which of several
    /// copies was heard.
    ///
    /// `None` for a file the library cannot place — it still plays.
    pub playing: Option<Id<Encoding>>,
    pub active: ActiveSource,
    pub aaa_mode: AaaMode,
    pub shuffle: bool,
    pub repeat: bool,
    pub repeat_current: bool,
    pub rng_state: u64,
}

impl Playback {
    /// Advance the shuffle order. Called after a `Wrapped` result so the next
    /// lap is not a rerun of the last one.
    pub fn reshuffle(&mut self) {
        self.rng_state = split_mix64(self.rng_state.wrapping_add(1));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{Scan, Track};
    use std::path::PathBuf;

    /// Three artists: A with two albums (2 + 1 tracks), B with one (2), C with
    /// one (1). Six tracks, so pools of several sizes are available.
    fn fixture() -> (Scan, Shelf) {
        let mut store = Scan::new();
        for (artist, album, n, title) in [
            ("A Artist", "First", 1u32, "a-first-1"),
            ("A Artist", "First", 2, "a-first-2"),
            ("A Artist", "Second", 1, "a-second-1"),
            ("B Artist", "Only", 1, "b-only-1"),
            ("B Artist", "Only", 2, "b-only-2"),
            ("C Artist", "Solo", 1, "c-solo-1"),
        ] {
            store.upsert(crate::store::heard(Track {
                path: PathBuf::from(format!("/m/{title}.mp3")),
                artist: Some(artist.into()),
                album: Some(album.into()),
                title: Some(title.into()),
                tracknumber: Some(n),
                ..Default::default()
            }));
        }
        let lib = Shelf::rebuild(&store, true);
        (store, lib)
    }

    fn id_of(store: &Scan, title: &str) -> TrackId {
        store
            .iter()
            .find(|(_, t)| t.title.as_deref() == Some(title))
            .map(|(id, _)| id)
            .unwrap_or_else(|| panic!("no track titled {title}"))
    }

    fn title_of(store: &Scan, id: TrackId) -> &str {
        store.get(id).unwrap().title.as_deref().unwrap()
    }

    struct Ctx {
        store: Scan,
        library: Shelf,
        playlist: Playlist,
        queue: Queue,
    }

    impl Ctx {
        fn new() -> Self {
            let (store, library) = fixture();
            Ctx {
                store,
                library,
                playlist: Playlist::new(),
                queue: Queue::new(),
            }
        }

        fn input(&self, p: Playback) -> NextInput<'_> {
            NextInput {
                queue: &self.queue,
                active: p.active,
                library: &self.library,
                playlist: &self.playlist,
                current: p.current,
                aaa_mode: p.aaa_mode,
                shuffle: p.shuffle,
                repeat: p.repeat,
                repeat_current: p.repeat_current,
                rng_state: p.rng_state,
            }
        }

        fn at(&self, title: &str) -> Playback {
            Playback {
                current: Some(id_of(&self.store, title)),
                ..Default::default()
            }
        }
    }

    // ---- the projected future (#56) --------------------------------------

    /// The first thing `upcoming` has to agree with is `next()` itself: its
    /// head must be the track that would actually play.
    #[test]
    fn the_projection_agrees_with_next() {
        let mut ctx = Ctx::new();
        for (label, p) in [
            ("in order", ctx.at("a-first-1")),
            (
                "shuffled",
                Playback {
                    shuffle: true,
                    rng_state: 42,
                    ..ctx.at("a-first-1")
                },
            ),
            (
                "nothing playing",
                Playback {
                    current: None,
                    ..Default::default()
                },
            ),
        ] {
            let head = upcoming(&ctx.input(p), 8).first().map(|(id, _)| *id);
            assert_eq!(
                head,
                next(ctx.input(p)),
                "{label}: the head is not what plays"
            );
        }

        // …and with a queued track in front, which wins unconditionally.
        ctx.queue.push_back(id_of(&ctx.store, "c-solo-1"));
        let p = ctx.at("a-first-1");
        let projection = upcoming(&ctx.input(p), 8);
        assert_eq!(projection[0].1, Upcoming::Queued);
        assert_eq!(projection.first().map(|(id, _)| *id), next(ctx.input(p)));
    }

    /// The queue comes first and is marked as such, because those rows are the
    /// only ones a future edit could reorder.
    #[test]
    fn the_queue_leads_and_is_marked() {
        let mut ctx = Ctx::new();
        ctx.queue.push_back(id_of(&ctx.store, "c-solo-1"));
        ctx.queue.push_back(id_of(&ctx.store, "b-only-2"));

        let out = upcoming(&ctx.input(ctx.at("a-first-1")), 8);
        let titles: Vec<&str> = out
            .iter()
            .map(|(id, _)| title_of(&ctx.store, *id))
            .collect();
        assert_eq!(&titles[..2], &["c-solo-1", "b-only-2"]);
        assert_eq!(out[0].1, Upcoming::Queued);
        assert_eq!(out[1].1, Upcoming::Queued);
        assert_eq!(out[2].1, Upcoming::Pool, "the pool should follow the queue");
    }

    /// Shuffled, the projection is the permutation's tail — which is the whole
    /// reason this is cheap. A played-set could not have answered this at all.
    #[test]
    fn shuffled_projects_the_rest_of_the_permutation() {
        let ctx = Ctx::new();
        let p = Playback {
            shuffle: true,
            rng_state: 7,
            ..ctx.at("a-first-1")
        };
        let projected: Vec<TrackId> = upcoming(&ctx.input(p), 8)
            .iter()
            .map(|(id, _)| *id)
            .collect();

        // Walk `next()` by hand and check it takes exactly that path.
        let mut walked = Vec::new();
        let mut at = p;
        for _ in 0..projected.len() {
            let Some(id) = next(ctx.input(at)) else { break };
            walked.push(id);
            at = Playback {
                current: Some(id),
                ..at
            };
        }
        assert_eq!(projected, walked, "the projection is not the path taken");
    }

    /// Without `repeat` the list simply ends — no wrap row, nothing invented.
    #[test]
    fn the_projection_ends_when_the_pool_does() {
        let ctx = Ctx::new();
        let out = upcoming(&ctx.input(ctx.at("c-solo-1")), 8);
        assert!(out.is_empty(), "c-solo-1 is last in order: {out:?}");
    }

    /// With `repeat` it shows **one** row past the wrap and stops. Going
    /// further would be a lie when shuffling: `Source::Wrapped` advances the
    /// seed, so the second lap is a different permutation from the one shown.
    #[test]
    fn repeat_shows_one_row_past_the_wrap_and_no_more() {
        let ctx = Ctx::new();
        let p = Playback {
            repeat: true,
            ..ctx.at("c-solo-1")
        };
        let out = upcoming(&ctx.input(p), 8);
        assert_eq!(out.len(), 1, "only the wrap row: {out:?}");
        assert_eq!(out[0].1, Upcoming::Wraps);
        assert_eq!(out.first().map(|(id, _)| *id), next(ctx.input(p)));
    }

    /// `repeat_current` means nothing else is coming, and saying so is more
    /// useful than listing a future that will not happen.
    #[test]
    fn repeat_current_projects_nothing() {
        let ctx = Ctx::new();
        let p = Playback {
            repeat_current: true,
            ..ctx.at("a-first-1")
        };
        assert!(upcoming(&ctx.input(p), 8).is_empty());
    }

    /// `repeat` makes the true answer infinite, so the limit is the contract.
    #[test]
    fn the_limit_is_honoured() {
        let mut ctx = Ctx::new();
        for t in ["c-solo-1", "b-only-1", "b-only-2"] {
            ctx.queue.push_back(id_of(&ctx.store, t));
        }
        let p = Playback {
            repeat: true,
            ..ctx.at("a-first-1")
        };
        for limit in [0, 1, 2, 4] {
            assert_eq!(upcoming(&ctx.input(p), limit).len(), limit, "limit {limit}");
        }
    }

    // ---- the empty and trivial cases ------------------------------------

    #[test]
    fn empty_library_yields_nothing() {
        let store = Scan::new();
        let library = Shelf::rebuild(&store, true);
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let p = Playback::default();
        let input = NextInput {
            queue: &queue,
            active: p.active,
            library: &library,
            playlist: &playlist,
            current: None,
            aaa_mode: p.aaa_mode,
            shuffle: false,
            repeat: true,
            repeat_current: false,
            rng_state: 0,
        };
        assert_eq!(next(input), None, "an empty library must stop, not loop");
    }

    #[test]
    fn single_track_stops_without_repeat_and_loops_with_it() {
        let mut store = Scan::new();
        store.upsert(crate::store::heard(Track {
            path: PathBuf::from("/m/only.mp3"),
            title: Some("only".into()),
            ..Default::default()
        }));
        let library = Shelf::rebuild(&store, true);
        let (playlist, queue) = (Playlist::new(), Queue::new());
        let only = id_of(&store, "only");

        let make = |repeat| NextInput {
            queue: &queue,
            active: ActiveSource::Library,
            library: &library,
            playlist: &playlist,
            current: Some(only),
            aaa_mode: AaaMode::All,
            shuffle: false,
            repeat,
            repeat_current: false,
            rng_state: 0,
        };
        assert_eq!(next(make(false)), None);
        assert_eq!(next(make(true)), Some(only));
    }

    #[test]
    fn nothing_playing_starts_at_the_top() {
        let ctx = Ctx::new();
        let got = next(ctx.input(Playback::default())).unwrap();
        assert_eq!(title_of(&ctx.store, got), "a-first-1");
    }

    // ---- the queue -------------------------------------------------------

    #[test]
    fn queue_wins_over_everything() {
        let mut ctx = Ctx::new();
        let queued = id_of(&ctx.store, "c-solo-1");
        ctx.queue.push_back(queued);

        // Queued track is returned regardless of shuffle, aaa_mode, or which
        // source is active — §5's "unconditionally".
        for (shuffle, aaa, active) in [
            (false, AaaMode::All, ActiveSource::Library),
            (true, AaaMode::All, ActiveSource::Library),
            (false, AaaMode::Album, ActiveSource::Library),
            (true, AaaMode::Artist, ActiveSource::Playlist),
        ] {
            let p = Playback {
                current: Some(id_of(&ctx.store, "a-first-1")),
                shuffle,
                aaa_mode: aaa,
                active,
                rng_state: 7,
                ..Default::default()
            };
            let (got, source) = next_with_source(ctx.input(p)).unwrap();
            assert_eq!(got, queued, "queue lost to {aaa:?}/{active:?}");
            assert_eq!(source, Source::Queue);
        }
    }

    #[test]
    fn repeat_current_beats_even_the_queue() {
        let mut ctx = Ctx::new();
        ctx.queue.push_back(id_of(&ctx.store, "c-solo-1"));
        let current = id_of(&ctx.store, "a-first-2");
        let p = Playback {
            current: Some(current),
            repeat_current: true,
            ..Default::default()
        };
        let (got, source) = next_with_source(ctx.input(p)).unwrap();
        assert_eq!(got, current);
        assert_eq!(source, Source::RepeatCurrent);
    }

    /// Playing a queued track must not move the library cursor: once the queue
    /// drains, play resumes after whatever the library was on.
    #[test]
    fn queue_does_not_move_the_library_cursor() {
        let mut ctx = Ctx::new();
        ctx.queue.push_back(id_of(&ctx.store, "c-solo-1"));
        let p = ctx.at("a-first-1");

        assert_eq!(next(ctx.input(p)).unwrap(), id_of(&ctx.store, "c-solo-1"));

        // Caller pops, and leaves `current` alone.
        ctx.queue.pop_front();
        let got = next(ctx.input(p)).unwrap();
        assert_eq!(
            title_of(&ctx.store, got),
            "a-first-2",
            "library resumed from the wrong place"
        );
    }

    // ---- aaa_mode --------------------------------------------------------

    #[test]
    fn aaa_all_walks_the_whole_library() {
        let ctx = Ctx::new();
        let mut p = ctx.at("a-first-1");
        let mut seen = vec!["a-first-1".to_string()];
        while let Some(id) = next(ctx.input(p)) {
            seen.push(title_of(&ctx.store, id).to_string());
            p.current = Some(id);
        }
        assert_eq!(
            seen,
            [
                "a-first-1",
                "a-first-2",
                "a-second-1",
                "b-only-1",
                "b-only-2",
                "c-solo-1"
            ]
        );
    }

    #[test]
    fn aaa_album_stops_at_the_album_boundary() {
        let ctx = Ctx::new();
        let p = Playback {
            aaa_mode: AaaMode::Album,
            ..ctx.at("a-first-2")
        };
        // "First" has two tracks; we are on the last of them.
        assert_eq!(next(ctx.input(p)), None);

        let p = Playback {
            aaa_mode: AaaMode::Album,
            ..ctx.at("a-first-1")
        };
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "a-first-2"
        );
    }

    #[test]
    fn aaa_artist_crosses_albums_but_not_artists() {
        let ctx = Ctx::new();
        // Last track of "First", same artist has "Second" after it.
        let p = Playback {
            aaa_mode: AaaMode::Artist,
            ..ctx.at("a-first-2")
        };
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "a-second-1"
        );

        // Last track of that artist: stop rather than crossing to B.
        let p = Playback {
            aaa_mode: AaaMode::Artist,
            ..ctx.at("a-second-1")
        };
        assert_eq!(next(ctx.input(p)), None);
    }

    /// §5 asks specifically about `aaa_mode` transitions when the current
    /// track changes artist — the pool must be recomputed from whatever is
    /// playing *now*, not from where playback started.
    #[test]
    fn aaa_pool_follows_the_current_track_across_artists() {
        let ctx = Ctx::new();
        let p = Playback {
            aaa_mode: AaaMode::Artist,
            ..ctx.at("b-only-1")
        };
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "b-only-2"
        );

        // Same mode, different current track: a different pool entirely.
        let p = Playback {
            aaa_mode: AaaMode::Artist,
            ..ctx.at("c-solo-1")
        };
        assert_eq!(
            next(ctx.input(p)),
            None,
            "C has one track; artist pool ends"
        );
    }

    #[test]
    fn aaa_narrowing_with_nothing_playing_falls_back_to_the_whole_library() {
        let ctx = Ctx::new();
        let p = Playback {
            aaa_mode: AaaMode::Album,
            current: None,
            ..Default::default()
        };
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "a-first-1",
            "narrowing with no current track must not deadlock playback"
        );
    }

    // ---- shuffle ---------------------------------------------------------

    #[test]
    fn shuffle_visits_every_track_exactly_once_before_stopping() {
        let ctx = Ctx::new();
        let p = Playback {
            shuffle: true,
            rng_state: 12345,
            current: None,
            ..Default::default()
        };
        let mut p = p;
        let mut seen = Vec::new();
        while let Some(id) = next(ctx.input(p)) {
            assert!(
                !seen.contains(&id),
                "shuffle repeated a track before exhausting the pool"
            );
            seen.push(id);
            p.current = Some(id);
        }
        assert_eq!(seen.len(), 6, "shuffle did not reach every track");
    }

    #[test]
    fn shuffle_order_is_stable_for_a_seed_and_differs_across_seeds() {
        let ctx = Ctx::new();
        let walk = |seed: u64| {
            let mut p = Playback {
                shuffle: true,
                rng_state: seed,
                ..Default::default()
            };
            let mut out = Vec::new();
            while let Some(id) = next(ctx.input(p)) {
                out.push(id);
                p.current = Some(id);
            }
            out
        };
        assert_eq!(walk(42), walk(42), "same seed must give the same order");
        assert_ne!(walk(1), walk(2), "different seeds should differ");
    }

    #[test]
    fn shuffle_exhaustion_wraps_only_with_repeat() {
        let ctx = Ctx::new();
        let mut p = Playback {
            shuffle: true,
            rng_state: 99,
            ..Default::default()
        };
        // Walk to the last track of the shuffled order.
        let mut last = None;
        while let Some(id) = next(ctx.input(p)) {
            last = Some(id);
            p.current = Some(id);
        }
        assert!(last.is_some());

        // Without repeat: stop.
        assert_eq!(next(ctx.input(p)), None);

        // With repeat: wrap, and say so, so the caller can reshuffle.
        let p = Playback { repeat: true, ..p };
        let (_, source) = next_with_source(ctx.input(p)).unwrap();
        assert_eq!(source, Source::Wrapped);
    }

    #[test]
    fn reshuffle_changes_the_order() {
        let ctx = Ctx::new();
        let mut p = Playback {
            shuffle: true,
            rng_state: 5,
            ..Default::default()
        };
        let first = next(ctx.input(p));
        p.reshuffle();
        let after = next(ctx.input(p));
        // Not a guarantee for every seed, but must at least change the state.
        assert_ne!(p.rng_state, 5);
        let _ = (first, after);
    }

    #[test]
    fn queue_during_shuffle_does_not_disturb_the_shuffle_order() {
        let mut ctx = Ctx::new();
        let p = Playback {
            shuffle: true,
            rng_state: 2024,
            ..Default::default()
        };

        // What shuffle would have given, with an empty queue.
        let expected = next(ctx.input(p)).unwrap();

        // Queue something; it wins.
        let queued = id_of(&ctx.store, "b-only-2");
        ctx.queue.push_back(queued);
        assert_eq!(next(ctx.input(p)).unwrap(), queued);

        // Drain it: the shuffle order picks up exactly where it was.
        ctx.queue.pop_front();
        assert_eq!(next(ctx.input(p)).unwrap(), expected);
    }

    // ---- prev ------------------------------------------------------------

    #[test]
    fn prev_walks_backwards() {
        let ctx = Ctx::new();
        let p = ctx.at("b-only-1");
        assert_eq!(
            title_of(&ctx.store, prev(ctx.input(p)).unwrap()),
            "a-second-1"
        );
    }

    #[test]
    fn prev_at_pool_start_stops_or_wraps() {
        let ctx = Ctx::new();
        let p = ctx.at("a-first-1");
        assert!(next(ctx.input(p)).is_some());
        assert_eq!(prev(ctx.input(p)), None, "should stop at the top");

        let p = Playback { repeat: true, ..p };
        assert_eq!(
            title_of(&ctx.store, prev(ctx.input(p)).unwrap()),
            "c-solo-1",
            "repeat should wrap to the end"
        );
    }

    #[test]
    fn prev_ignores_the_queue() {
        let mut ctx = Ctx::new();
        ctx.queue.push_back(id_of(&ctx.store, "c-solo-1"));
        let p = ctx.at("b-only-1");
        assert_eq!(
            title_of(&ctx.store, prev(ctx.input(p)).unwrap()),
            "a-second-1",
            "prev must not consult the queue"
        );
    }

    #[test]
    fn prev_honours_repeat_current() {
        let ctx = Ctx::new();
        let p = Playback {
            repeat_current: true,
            ..ctx.at("b-only-1")
        };
        assert_eq!(prev(ctx.input(p)), p.current);
    }

    // ---- playlist source -------------------------------------------------

    #[test]
    fn playlist_source_walks_the_playlist_in_order_including_duplicates() {
        let mut ctx = Ctx::new();
        let a = id_of(&ctx.store, "c-solo-1");
        let b = id_of(&ctx.store, "a-first-1");
        ctx.playlist.push(a);
        ctx.playlist.push(b);
        ctx.playlist.push(a);

        let p = Playback {
            active: ActiveSource::Playlist,
            current: None,
            ..Default::default()
        };
        assert_eq!(next(ctx.input(p)).unwrap(), a);

        // A duplicate resolves to the first occurrence, so playback advances
        // rather than sticking — the known cost of identifying position by id.
        let p = Playback {
            active: ActiveSource::Playlist,
            current: Some(a),
            ..Default::default()
        };
        assert_eq!(next(ctx.input(p)).unwrap(), b);
    }

    #[test]
    fn playlist_ignores_aaa_mode() {
        let mut ctx = Ctx::new();
        ctx.playlist.push(id_of(&ctx.store, "a-first-1"));
        ctx.playlist.push(id_of(&ctx.store, "c-solo-1"));
        let p = Playback {
            active: ActiveSource::Playlist,
            aaa_mode: AaaMode::Album,
            current: Some(id_of(&ctx.store, "a-first-1")),
            ..Default::default()
        };
        // Album narrowing would have stopped at the album boundary; the
        // playlist crosses it, because §5 applies aaa only to the library.
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "c-solo-1"
        );
    }

    #[test]
    fn empty_playlist_stops() {
        let ctx = Ctx::new();
        let p = Playback {
            active: ActiveSource::Playlist,
            repeat: true,
            ..Default::default()
        };
        assert_eq!(next(ctx.input(p)), None);
    }

    // ---- odd states ------------------------------------------------------

    /// The current track can vanish from the pool: it was queued, or the mode
    /// changed under it. Restarting the pool beats stopping dead.
    #[test]
    fn current_outside_the_pool_restarts_the_pool() {
        let mut ctx = Ctx::new();
        ctx.playlist.push(id_of(&ctx.store, "b-only-1"));
        let p = Playback {
            active: ActiveSource::Playlist,
            current: Some(id_of(&ctx.store, "c-solo-1")), // not in the playlist
            ..Default::default()
        };
        assert_eq!(
            title_of(&ctx.store, next(ctx.input(p)).unwrap()),
            "b-only-1"
        );
    }

    #[test]
    fn aaa_mode_cycles_through_all_three() {
        assert_eq!(AaaMode::All.cycle(), AaaMode::Artist);
        assert_eq!(AaaMode::Artist.cycle(), AaaMode::Album);
        assert_eq!(AaaMode::Album.cycle(), AaaMode::All);
    }

    #[test]
    fn shuffled_is_a_permutation() {
        let pool: Vec<TrackId> = (0..50).map(crate::store::row_of_test).collect();
        // Sorted on both sides. A row id used to be an arena index, so a pool
        // built from `0..50` was already in order and only the result needed
        // sorting; a derived id has no such order (#86).
        let mut want = pool.clone();
        want.sort();
        for seed in [0u64, 1, 42, u64::MAX] {
            let mut got = shuffled(&pool, seed);
            got.sort();
            assert_eq!(got, want, "seed {seed} lost or duplicated a track");
        }
    }
}
