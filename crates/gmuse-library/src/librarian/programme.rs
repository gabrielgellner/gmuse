//! **The door to the programme, and the cues that come back through it** — #257.
//!
//! [`crate::programme::Programme`] is the ordering; this is how anything else reaches it.
//! One typed ask, one typed reply, answered on the librarian thread — which is the only
//! thread that has both halves the answer needs: the programme, and the fold that turns a
//! row into a path.
//!
//! ## Why the engine asks rather than holding a pool
//!
//! §4.4 forbids the control thread from running a query, and `NeedNext` has a real
//! deadline — the decoder asks while the current track is still playing. `queue.rs`
//! answered that by materialising the whole pool in the engine, which is why the engine
//! holds a fold at all.
//!
//! **A window is the better answer to the same problem.** The engine holds the next two or
//! three cues, so `NeedNext` is a local lookup with no ask at all; the refill happens
//! afterwards, off the deadline. Less memory than a materialised pool and less latency
//! than asking on the deadline.
//!
//! ## Nothing here is shared with the engine
//!
//! A token is opaque, so it crosses as a `u64`. [`Mode`] never crosses at all — the engine
//! cannot tell which mode is in force, which is the whole decision. So this vocabulary
//! needs no type in `gmuse-ids` or `gmuse-proto`, and the engine will name none of it.
//!
//! [`Mode`]: crate::programme::Mode

use std::path::PathBuf;

use gmuse_ids::copy::CopyId;

use crate::model::rows::Listing;
use crate::programme::Mode;

/// What to do to the programme, or ask of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ask {
    /// **Start over on these rows**, and cue the first `window` of them.
    ///
    /// Every token the caller held stops matching, which is the honest answer for a
    /// genuinely different list.
    Play {
        rows: Vec<CopyId>,
        window: usize,
    },
    /// **Build a programme from the library and cue from one row** — a play gesture.
    ///
    /// The caller names a row, a narrowing, and whether to shuffle; the librarian
    /// materialises the pool, orders it, and cues from `at`. **The pool never crosses**,
    /// which is the point of the ask existing: scope `All` is 7,271 rows and a caller has
    /// no business holding them.
    ///
    /// `at` absent cues from the head — "play this album" rather than "play this track".
    Open {
        at: Option<CopyId>,
        scope: crate::next::Scope,
        shuffle: bool,
        /// Seeds the permutation. **The same seed shuffles the same pool the same way**, so
        /// a reshuffle is a new seed rather than a hidden `rand` call, and a test can assert
        /// an order.
        seed: u64,
        window: usize,
    },
    /// **The `n` cues after this token** — the refill.
    ///
    /// `version` is what the caller last saw. A mismatch is not by itself an error: the
    /// token is looked up regardless, because a reorder keeps tokens and the caller's
    /// position is still good. The version is reported back so a caller can tell a
    /// reorder from a replace.
    After {
        token: u64,
        n: usize,
    },
    /// The same rows in a new order, keeping every token. A reshuffle.
    Reorder {
        order: Vec<u64>,
    },
    /// What happens at the end of an entry.
    SetMode(Mode),
    Insert {
        at: usize,
        rows: Vec<CopyId>,
    },
    Remove {
        tokens: Vec<u64>,
    },
    Clear,
    /// **The programme as it stands, for drawing it.** Up to `limit` entries.
    Look {
        limit: usize,
    },
}

/// Where to find one entry's audio, and what the tags say about its loudness.
#[derive(Debug, Clone, PartialEq)]
pub struct Cued {
    /// The programme entry. **Opaque**: hand it back, do not read it.
    pub token: u64,
    pub path: PathBuf,
    /// The ReplayGain tags, in dB, and nothing done with them.
    ///
    /// **How loud to play is the engine's question**, because it is the half that holds
    /// the person's gain preference; what the tags say is the library's. `Playable`'s own
    /// comment has drawn that line since #78, and `source_for` plays at unity today
    /// precisely because the cue carried no way to say this.
    pub track_db: Option<f32>,
    pub track_peak: Option<f32>,
    pub album_db: Option<f32>,
    pub album_peak: Option<f32>,
}

/// One entry, for a surface that draws the programme rather than plays it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shown {
    pub token: u64,
    pub row: CopyId,
}

/// What came back.
#[derive(Debug, Clone, PartialEq)]
pub enum Reply {
    /// Cues to play, in order, and the version they were taken from.
    ///
    /// **Shorter than asked for is not an error.** A `OnceThrough` programme runs out, and
    /// a row whose audio this device does not hold is skipped rather than cueing a path
    /// that cannot be opened.
    Window { version: u64, cues: Vec<Cued> },
    /// **The token is not in this programme.** Ask for a window from the top.
    ///
    /// The rarer case than the design document assumed, because a reshuffle is a
    /// [`Ask::Reorder`] and keeps every token — so this means a genuine replacement.
    Stale { version: u64 },
    /// The programme, for drawing.
    Look {
        version: u64,
        mode: Mode,
        entries: Vec<Shown>,
        /// How many the programme holds, which `entries` may be a `limit`ed prefix of.
        total: usize,
    },
    /// A mutation landed. The version it landed at.
    Settled { version: u64 },
}

/// Apply one ask to the programme, resolving rows to paths through the fold.
///
/// **Called only from the librarian thread**, which is what makes it the single decider of
/// what plays next. It takes the fold rather than holding one, so it cannot go stale
/// against what the librarian published.
pub(crate) fn answer(
    plan: &mut crate::programme::Programme,
    library: &crate::model::seam::Librarian,
    device: crate::model::id::Id<crate::model::entity::Device>,
    ask: Ask,
) -> Reply {
    use crate::programme::EntryToken;

    match ask {
        Ask::Play { rows, window } => {
            plan.replace(rows);
            Reply::Window {
                version: plan.version().as_u64(),
                cues: cues_for(plan.head(window), library, device),
            }
        }

        Ask::Open {
            at,
            scope,
            shuffle,
            seed,
            window,
        } => {
            let ordered = pool(library, at, scope, shuffle, seed);
            plan.replace(ordered);
            let version = plan.version().as_u64();

            // **Cue from the row the person picked, not from the head.** `at` may be
            // absent (play the album) or no longer in the pool at all — a scope narrowed
            // under it, or it was forgotten — and starting at the top is what `next.rs`
            // has always done in that case, for the same reason: it is the least
            // surprising thing that is still playing something.
            let entries = match at.and_then(|row| plan.first_of(row)) {
                Some(entry) => plan.window_at(entry.token, window),
                None => plan.head(window),
            };
            Reply::Window {
                version,
                cues: cues_for(entries, library, device),
            }
        }

        Ask::After { token, n } => {
            let token = EntryToken::from_u64(token);
            if plan.entry(token).is_none() {
                return Reply::Stale {
                    version: plan.version().as_u64(),
                };
            }
            Reply::Window {
                version: plan.version().as_u64(),
                cues: cues_for(plan.after(token, n), library, device),
            }
        }

        Ask::Reorder { order } => {
            let order: Vec<EntryToken> = order.into_iter().map(EntryToken::from_u64).collect();
            // **A refused reorder is reported as settled at the unchanged version**, not
            // as an error. The caller learns the version did not move, which is the same
            // thing it would learn from a no-op — and there is nothing it could usefully
            // do differently.
            plan.reorder(&order);
            Reply::Settled {
                version: plan.version().as_u64(),
            }
        }

        Ask::SetMode(mode) => {
            plan.set_mode(mode);
            Reply::Settled {
                version: plan.version().as_u64(),
            }
        }

        Ask::Insert { at, rows } => {
            plan.insert(at, rows);
            Reply::Settled {
                version: plan.version().as_u64(),
            }
        }

        Ask::Remove { tokens } => {
            let tokens: Vec<EntryToken> = tokens.into_iter().map(EntryToken::from_u64).collect();
            plan.remove(&tokens);
            Reply::Settled {
                version: plan.version().as_u64(),
            }
        }

        Ask::Clear => {
            plan.clear();
            Reply::Settled {
                version: plan.version().as_u64(),
            }
        }

        Ask::Look { limit } => Reply::Look {
            version: plan.version().as_u64(),
            mode: plan.mode(),
            entries: plan
                .entries()
                .iter()
                .take(limit)
                .map(|e| Shown {
                    token: e.token.as_u64(),
                    row: e.row,
                })
                .collect(),
            total: plan.len(),
        },
    }
}

/// **The pool, narrowed and ordered.**
///
/// It lived in the engine, which held a materialised `Shelf` of the whole library and
/// rebuilt it on every fold, because §4.4 forbids the control thread from running a query
/// and `NeedNext` has a deadline. Here there is no deadline: this runs once per play
/// gesture on the thread that already owns the fold, and what crosses afterwards is a
/// window of three.
///
/// **`next::scoped` and `next::shuffled` are called rather than reimplemented.** A ported
/// copy of the shuffle was written first and was wrong in a way nothing could have seen —
/// `next::split_mix64` does not advance its argument, so a canonical SplitMix64 produces a
/// different permutation from the same seed. One implementation is the only version of
/// that check which cannot rot.
fn pool(
    library: &crate::model::seam::Librarian,
    at: Option<CopyId>,
    scope: crate::next::Scope,
    shuffle: bool,
    seed: u64,
) -> Vec<CopyId> {
    let members: std::collections::HashSet<CopyId> = Listing::rows(library).into_iter().collect();
    let shelf = crate::shelf::Shelf::from_members(library, members, true);
    let narrowed = crate::next::scoped(&shelf, at, scope);
    if shuffle {
        crate::next::shuffled(&narrowed, seed)
    } else {
        narrowed
    }
}

/// Turn entries into cues, dropping any this device cannot open.
///
/// **Dropped rather than cued with an unopenable path.** A row whose audio lives on a
/// device that is not here is a real holding — `shelf copies` is about exactly that — and
/// handing the decoder a path it cannot open would turn it into an underrun rather than a
/// skip. The window simply comes back shorter.
fn cues_for(
    entries: Vec<crate::programme::Entry>,
    library: &crate::model::seam::Librarian,
    device: crate::model::id::Id<crate::model::entity::Device>,
) -> Vec<Cued> {
    entries
        .into_iter()
        .filter_map(|entry| {
            let recording = library.recording_of(entry.row)?;
            let (_encoding, locator) = library.open(recording, &[device], &[])?;
            let loudness = library.playable(entry.row).and_then(|p| p.gain);
            Some(Cued {
                token: entry.token.as_u64(),
                path: PathBuf::from(locator),
                track_db: loudness.and_then(|l| l.track_db),
                track_peak: loudness.and_then(|l| l.track_peak),
                album_db: loudness.and_then(|l| l.album_db),
                album_peak: loudness.and_then(|l| l.album_peak),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::instance::Scanned;
    use crate::operations::scan::Holdings;
    use crate::programme::{Mode, Programme};

    /// A library of `n` tracks on one album, held at predictable paths.
    fn library(n: u32) -> (crate::model::seam::Librarian, Vec<CopyId>) {
        let mut held = Holdings::new();
        for i in 1..=n {
            held.take(
                PathBuf::from(format!("/m/Luna/{i:02} Track.flac")),
                0,
                0,
                Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: i as u128,
                    },
                    title: format!("Track {i}"),
                    artist: "Stephan Bodzin".into(),
                    album: Some("Luna".into()),
                    track: Some(i as u16),
                    length_ms: 200_000,
                    levels: crate::model::encoding::Levels {
                        // -7.30 dB and a peak of 0.9876, so a mangled scale is visible.
                        track_cdb: Some(-730),
                        track_peak_e4: Some(9876),
                        ..Default::default()
                    },
                    ..Default::default()
                },
            );
        }
        let l = crate::fixture::from_holdings(&held);
        // **In the order a programme will hold them**, which is the shelf's — by who made
        // it, then the release, then the track. Sorting by `CopyId` instead sorts by a uuid
        // scalar, and `rows[2]` of four could then be the last entry a programme holds,
        // which is how two tests below first read "the window was short".
        let members: std::collections::HashSet<CopyId> =
            crate::model::rows::Listing::rows(&l).into_iter().collect();
        let rows = crate::shelf::Shelf::from_members(&l, members, true)
            .sorted()
            .to_vec();
        (l, rows)
    }

    fn ask(plan: &mut Programme, l: &crate::model::seam::Librarian, what: Ask) -> Reply {
        answer(plan, l, crate::fixture::device(), what)
    }

    /// **A cue carries a path this device can actually open** — the point of the librarian
    /// answering rather than the engine resolving.
    #[test]
    fn a_window_carries_real_paths() {
        let (l, rows) = library(3);
        assert_eq!(rows.len(), 3, "the fixture did not make three rows");
        let mut plan = Programme::new();

        let Reply::Window { cues, version } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows: rows.clone(),
                window: 2,
            },
        ) else {
            panic!("a play did not answer with a window");
        };

        assert_eq!(cues.len(), 2, "the window was not the size asked for");
        assert!(version > 0, "a play left the version at zero");
        for cue in &cues {
            assert!(
                cue.path.starts_with("/m/Luna/"),
                "a cue pointed somewhere else: {}",
                cue.path.display()
            );
        }
        assert_ne!(cues[0].token, cues[1].token, "one token for two entries");
    }

    /// **The loudness tags cross, unchanged and unapplied.**
    ///
    /// `source_for` plays every track at unity today because a cue had no way to say this,
    /// and the comment there calls a silent guess at the person's gain preference the worse
    /// error. The cue says what the tags are; the engine decides how loud.
    #[test]
    fn the_tags_reach_the_cue_without_being_applied() {
        let (l, rows) = library(1);
        let mut plan = Programme::new();

        let Reply::Window { cues, .. } = ask(&mut plan, &l, Ask::Play { rows, window: 1 }) else {
            panic!("no window");
        };
        let cue = &cues[0];
        assert_eq!(
            cue.track_db,
            Some(-7.30),
            "the track gain did not survive, or a scale was applied"
        );
        assert_eq!(cue.track_peak, Some(0.9876), "the peak did not survive");
        assert_eq!(cue.album_db, None, "an album gain was invented");
    }

    /// **A token this programme does not hold is reported, not guessed at.**
    ///
    /// The alternative — answering from the top — would silently restart a queue on a lost
    /// message, and the caller could not tell that from a real refill.
    #[test]
    fn a_stale_token_is_reported() {
        let (l, rows) = library(2);
        let mut plan = Programme::new();
        ask(&mut plan, &l, Ask::Play { rows, window: 1 });

        match ask(&mut plan, &l, Ask::After { token: 9_999, n: 2 }) {
            Reply::Stale { version } => assert!(version > 0),
            other => panic!("an unknown token was answered with {other:?}"),
        }
    }

    /// **A row the fold cannot resolve shortens the window rather than cueing a bad path.**
    ///
    /// Handing the decoder a path it cannot open is an underrun; handing it one fewer cue
    /// is a skip. The row is real — a programme can outlive a `forget` — so this is a case
    /// rather than a corruption.
    #[test]
    fn an_unresolvable_row_is_dropped_from_the_window() {
        let (l, rows) = library(2);
        let mut plan = Programme::new();

        let mut with_a_ghost = vec![gmuse_ids::copy::row_of_test(77)];
        with_a_ghost.extend(rows.iter().copied());

        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows: with_a_ghost,
                window: 3,
            },
        ) else {
            panic!("no window");
        };

        // The programme holds three entries; only two of them can be cued.
        assert_eq!(
            plan.len(),
            3,
            "the ghost was refused entry to the programme"
        );
        assert_eq!(cues.len(), 2, "the ghost was cued anyway");
        assert!(
            cues.iter().all(|c| c.path.starts_with("/m/Luna/")),
            "a cue pointed at nothing"
        );
    }

    /// **A `Look` says what is there, how much of it, and under which mode.**
    #[test]
    fn a_look_reports_the_mode_and_the_whole_length() {
        let (l, rows) = library(3);
        let mut plan = Programme::new();
        ask(&mut plan, &l, Ask::Play { rows, window: 1 });
        ask(&mut plan, &l, Ask::SetMode(Mode::RepeatAll));

        let Reply::Look {
            mode,
            entries,
            total,
            ..
        } = ask(&mut plan, &l, Ask::Look { limit: 2 })
        else {
            panic!("a look did not answer with one");
        };
        assert_eq!(mode, Mode::RepeatAll, "the mode was not reported");
        assert_eq!(entries.len(), 2, "the limit was not applied");
        assert_eq!(total, 3, "a limited look misreported the length");
    }

    /// **The mode is never in a cue**, which is the decision the whole design rests on.
    ///
    /// An engine that could read the mode would be an engine that could act on it, and the
    /// argument for a rule over a flag would be lost the first time something did.
    #[test]
    fn a_cue_says_nothing_about_the_mode() {
        let (l, rows) = library(1);
        let mut plan = Programme::new();
        ask(&mut plan, &l, Ask::SetMode(Mode::RepeatCurrent));
        let Reply::Window { cues, .. } = ask(&mut plan, &l, Ask::Play { rows, window: 2 }) else {
            panic!("no window");
        };

        // Repeat-current is in force, so a window of two over one entry is that entry
        // twice — which is the *behaviour* crossing, and the only way it should.
        assert_eq!(cues.len(), 2, "the mode did not shape the window");
        assert_eq!(
            cues[0].token, cues[1].token,
            "it cued two different entries"
        );

        // And nothing in the cue names it. A field added later would fail here.
        let rendered = format!("{:?}", cues[0]);
        for named in ["RepeatCurrent", "RepeatAll", "OnceThrough", "mode"] {
            assert!(
                !rendered.contains(named),
                "a cue carries {named}: {rendered}"
            );
        }
    }

    /// **The wiring, through the real `Desk`** — the job, the channel, the thread's state.
    ///
    /// Every test above calls `answer` directly, which proves the logic and nothing about
    /// whether anything reaches it. `Job::Programme` could be unsent, the thread's `plan`
    /// could be rebuilt per job, and all six would still pass — which is §13's second
    /// shape, a harness that silently does less than it claims.
    ///
    /// So this asserts the two things only the thread can be wrong about: the programme
    /// **persists across jobs**, and a reorder **keeps the caller's position** end to end.
    #[test]
    fn a_desk_holds_one_programme_across_jobs_and_a_reorder_keeps_your_place() {
        let mut held = Holdings::new();
        for i in 1..=3u32 {
            held.take(
                PathBuf::from(format!("/m/Luna/{i:02} Track.flac")),
                0,
                0,
                Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: i as u128,
                    },
                    title: format!("Track {i}"),
                    artist: "Stephan Bodzin".into(),
                    album: Some("Luna".into()),
                    track: Some(i as u16),
                    length_ms: 200_000,
                    ..Default::default()
                },
            );
        }
        let device = crate::fixture::device();
        let mut log = crate::model::instance::Instance::new(device);
        log.ingest(100, &held.tags());
        let desk = super::super::Desk::open(log, held, device, 100, None);

        let mut rows = crate::model::rows::Listing::rows(&desk.current().library);
        rows.sort();
        assert_eq!(
            rows.len(),
            3,
            "the desk folded no rows, so nothing below means anything"
        );

        // One job opens the programme. Two cues, so the *second* is playing — a first
        // entry reversed to last would answer `None` for a reason that has nothing to do
        // with the token surviving, which is what this test is about.
        let Some(Reply::Window { cues, .. }) = desk.programme(Ask::Play {
            rows: rows.clone(),
            window: 2,
        }) else {
            panic!("the desk did not answer a Play");
        };
        assert_eq!(cues.len(), 2);
        let playing = cues[1].token;

        // A second job sees the same programme — the thread kept it.
        let Some(Reply::Look { entries, total, .. }) = desk.programme(Ask::Look { limit: 10 })
        else {
            panic!("the desk did not answer a Look");
        };
        assert_eq!(total, 3, "the programme did not survive the first job");
        let order: Vec<u64> = entries.iter().map(|e| e.token).collect();
        assert!(
            order.contains(&playing),
            "the cued entry is not in the programme"
        );

        // A reshuffle: same rows, reversed. Every token stays valid.
        let reversed: Vec<u64> = order.iter().rev().copied().collect();
        let Some(Reply::Settled { .. }) = desk.programme(Ask::Reorder {
            order: reversed.clone(),
        }) else {
            panic!("the desk did not answer a Reorder");
        };

        // And the caller's position still resolves, which is the property that means a
        // person toggling shuffle does not hear the music stop.
        match desk.programme(Ask::After {
            token: playing,
            n: 1,
        }) {
            Some(Reply::Window { cues, .. }) => {
                assert_eq!(
                    cues.len(),
                    1,
                    "there was nothing after the reordered position"
                );
            }
            other => panic!("a reorder went stale on the caller: {other:?}"),
        }
    }

    /// **A reorder can move you to the end, and then nothing follows** — found by the test
    /// above asserting otherwise.
    ///
    /// The token survives, so the track that is sounding keeps sounding; but `OnceThrough`
    /// honestly reports that the new order has nothing after it. That is the arithmetic
    /// working, and it is also probably not what a person hitting shuffle expects — they
    /// expect the *rest* to be reshuffled, not the current track to be able to become the
    /// last one.
    ///
    /// **Which is an ordering-policy question, and therefore the librarian's**: "shuffle
    /// the remainder, pinning what is playing" is an argument to a reorder rather than a
    /// new mechanism. #235 is where a policy gets named. Recorded as a test rather than a
    /// comment so the behaviour is pinned either way — if it changes, this fails and says
    /// why.
    #[test]
    fn a_reorder_to_the_end_leaves_nothing_after_it() {
        let (l, rows) = library(3);
        let mut plan = Programme::new();
        let Reply::Window { cues, .. } = ask(&mut plan, &l, Ask::Play { rows, window: 1 }) else {
            panic!("no window");
        };
        let playing = cues[0].token;

        let held: Vec<u64> = plan.entries().iter().map(|e| e.token.as_u64()).collect();
        let reversed: Vec<u64> = held.iter().rev().copied().collect();
        ask(&mut plan, &l, Ask::Reorder { order: reversed });

        // Still valid — the token was carried, not re-minted.
        assert!(
            matches!(
                ask(&mut plan, &l, Ask::Look { limit: 10 }),
                Reply::Look { total: 3, .. }
            ),
            "the reorder did not keep three entries"
        );

        // But last, so once-through has nothing after it.
        match ask(
            &mut plan,
            &l,
            Ask::After {
                token: playing,
                n: 1,
            },
        ) {
            Reply::Window { cues, .. } => assert!(
                cues.is_empty(),
                "something followed the last entry of a once-through programme"
            ),
            other => panic!("the token went stale rather than running out: {other:?}"),
        }
    }

    /// **A play gesture builds the programme, and the picked row sounds first.**
    #[test]
    fn open_cues_from_the_row_that_was_picked() {
        let (l, rows) = library(4);
        let mut plan = Programme::new();

        let picked = rows[2];
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Open {
                at: Some(picked),
                scope: crate::next::Scope::All,
                shuffle: false,
                seed: 0,
                window: 2,
            },
        ) else {
            panic!("an open did not answer with a window");
        };

        assert_eq!(plan.len(), 4, "the whole pool did not become the programme");
        assert_eq!(cues.len(), 2, "the window was short");
        assert_eq!(
            plan.first_of(picked).map(|e| e.token.as_u64()),
            Some(cues[0].token),
            "the first cue was not the row that was picked"
        );
    }

    /// **The pool never crosses** — the whole reason `Open` exists rather than `Play`.
    ///
    /// Scope `All` is 7,271 rows on the real library. A caller that had to send them would
    /// be a caller holding a pool, which is what this design took away from the engine.
    #[test]
    fn open_sends_a_window_and_not_a_pool() {
        let (l, rows) = library(20);
        let mut plan = Programme::new();
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Open {
                at: Some(rows[0]),
                scope: crate::next::Scope::All,
                shuffle: false,
                seed: 0,
                window: 3,
            },
        ) else {
            panic!("no window");
        };
        assert_eq!(
            plan.len(),
            20,
            "the pool was not materialised librarian-side"
        );
        assert_eq!(cues.len(), 3, "more than a window crossed");
    }

    /// **The shuffle is `next::shuffled`, not a copy of it.**
    ///
    /// A ported copy was written first and was wrong invisibly: `next::split_mix64` does not
    /// advance its argument — the caller reassigns — so a canonical SplitMix64 that advances
    /// internally gives a different permutation from the same seed. Nothing in the program
    /// would have noticed, because no order is asserted anywhere else.
    ///
    /// So this asserts against `next::shuffled` directly. If `Ask::Open` ever grows its own
    /// copy, the two diverge and this says so.
    #[test]
    fn open_shuffles_exactly_as_next_does() {
        let (l, rows) = library(8);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Open {
                at: None,
                scope: crate::next::Scope::All,
                shuffle: true,
                seed: 12_345,
                window: 1,
            },
        );

        let members: std::collections::HashSet<CopyId> = rows.iter().copied().collect();
        let shelf = crate::shelf::Shelf::from_members(&l, members, true);
        let expected = crate::next::shuffled(shelf.sorted(), 12_345);

        let got: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();
        assert_eq!(got.len(), 8, "the shuffled pool lost rows");
        assert_ne!(
            got,
            shelf.sorted().to_vec(),
            "a seed of 12345 left eight rows in sorted order, so shuffle did nothing"
        );
        assert_eq!(got, expected, "the order diverged from next::shuffled");
    }

    /// **A narrowing with nothing to narrow to falls back to the whole pool**, rather than
    /// playing nothing.
    ///
    /// `next.rs` has always done this and says why: a scope that silently played nothing
    /// would look like a broken player. Asserted here because `Ask::Open` is a second
    /// caller of the rule and a fallback is the easiest thing to drop.
    #[test]
    fn a_narrowing_with_no_row_to_narrow_to_keeps_the_whole_pool() {
        let (l, _rows) = library(5);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Open {
                at: None,
                scope: crate::next::Scope::Inner,
                shuffle: false,
                seed: 0,
                window: 1,
            },
        );
        assert_eq!(
            plan.len(),
            5,
            "an unanchored narrowing emptied the programme"
        );
    }
}
