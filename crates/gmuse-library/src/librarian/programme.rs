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

/// **The vocabulary, from `gmuse-programme`** — #257.
///
/// The words moved to the crate both actors can name; what stayed is everything that needs
/// the fold. `answer` resolves a row to a path and `pool` projects a shelf, and neither is
/// something the audio engine may do.
pub use gmuse_programme::{Ask, Cued, Reply, Shown, Standing};

use crate::model::rows::Listing;

/// Apply one ask to the programme, resolving rows to paths through the fold.
///
/// **Called only from the librarian thread**, which is what makes it the single decider of
/// what plays next. It takes the fold rather than holding one, so it cannot go stale
/// against what the librarian published.
/// What is true of the programme right now.
fn standing(plan: &crate::programme::Programme) -> Standing {
    Standing {
        version: plan.version().as_u64(),
        mode: plan.mode(),
        scope: plan.scope(),
        shuffled: plan.shuffled(),
        total: plan.len(),
    }
}

pub(crate) fn answer(
    plan: &mut crate::programme::Programme,
    library: &crate::model::seam::Librarian,
    device: crate::model::id::Id<crate::model::entity::Device>,
    ask: Ask,
) -> Reply {
    use crate::programme::EntryToken;

    // **#233's optimistic concurrency, in one place.** A mutation computed against a
    // version the programme has moved past is refused rather than applied to something
    // else — two views reordering at once is a lost update, and §8.8 says in as many words
    // that this is the state where that matters. A `None` base is a caller that did not
    // care, which the wire allows.
    if let Some(base) = ask.base()
        && base != plan.version().as_u64()
    {
        return Reply::Stale {
            standing: standing(plan),
        };
    }

    match ask {
        Ask::Play { rows, window, .. } => {
            plan.replace(rows);
            Reply::Window {
                standing: standing(plan),
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
            plan.made_by(scope, shuffle);

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
                standing: standing(plan),
                cues: cues_for(entries, library, device),
            }
        }

        Ask::Start { recording, window } => {
            // **A scan of the rows, and that is the right answer for one.** #123's entry is
            // the precedent: building an index of every recording to ask about one track is
            // worse than walking them, and pressing play happens at human speed.
            let at = Listing::rows(library)
                .into_iter()
                .find(|row| library.recording_of(*row) == Some(recording));
            answer(
                plan,
                library,
                device,
                Ask::Open {
                    at,
                    scope: plan.scope(),
                    shuffle: plan.shuffled(),
                    seed: 0,
                    window,
                },
            )
        }

        Ask::Tail { n } => {
            let entries = match plan.entries().last().copied() {
                Some(last) => {
                    let mut out = vec![last];
                    out.extend(plan.before(last.token, n.saturating_sub(1)));
                    out
                }
                None => Vec::new(),
            };
            Reply::Window {
                standing: standing(plan),
                cues: cues_for(entries, library, device),
            }
        }

        Ask::Head { n } => Reply::Window {
            standing: standing(plan),
            cues: cues_for(plan.head(n), library, device),
        },

        Ask::After { token, n } => {
            let token = EntryToken::from_u64(token);
            if plan.entry(token).is_none() {
                return Reply::Stale {
                    standing: standing(plan),
                };
            }
            Reply::Window {
                standing: standing(plan),
                cues: cues_for(plan.after(token, n), library, device),
            }
        }

        Ask::Before { token, n } => {
            let token = EntryToken::from_u64(token);
            if plan.entry(token).is_none() {
                return Reply::Stale {
                    standing: standing(plan),
                };
            }
            Reply::Window {
                standing: standing(plan),
                cues: cues_for(plan.before(token, n), library, device),
            }
        }

        Ask::Shuffle { on, seed } => {
            // **The rows already in the programme, ordered** — not the pool. Shuffling
            // re-derived from the library would silently drop whatever was put in by hand,
            // and re-including what was taken out.
            let rows: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();
            let members: std::collections::HashSet<CopyId> = rows.iter().copied().collect();
            let shelf = crate::shelf::Shelf::from_members(library, members, true);
            let ordered = if on {
                crate::next::shuffled(shelf.sorted(), seed)
            } else {
                shelf.sorted().to_vec()
            };
            plan.reorder_by_row(&ordered);
            plan.made_by(plan.scope(), on);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Rescope {
            at,
            scope,
            shuffle,
            seed,
            window,
        } => {
            // The caller holds a token, not a row: only it knows which entry is sounding.
            let anchor = at
                .map(EntryToken::from_u64)
                .and_then(|t| plan.entry(t))
                .map(|e| e.row);
            answer(
                plan,
                library,
                device,
                Ask::Open {
                    at: anchor,
                    scope,
                    shuffle,
                    seed,
                    window,
                },
            )
        }

        Ask::Reorder { order } => {
            let order: Vec<EntryToken> = order.into_iter().map(EntryToken::from_u64).collect();
            // **A refused reorder is reported as settled at the unchanged version**, not
            // as an error. The caller learns the version did not move, which is the same
            // thing it would learn from a no-op — and there is nothing it could usefully
            // do differently.
            plan.reorder(&order);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::SetMode(mode) => {
            plan.set_mode(mode);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Insert { at, rows } => {
            plan.insert(at, rows);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Remove { tokens } => {
            let tokens: Vec<EntryToken> = tokens.into_iter().map(EntryToken::from_u64).collect();
            plan.remove(&tokens);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Push { rows, .. } => {
            plan.push(rows);
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::RemoveAt { index, .. } => {
            let token = plan.entries().get(index).map(|e| e.token);
            if let Some(token) = token {
                plan.remove(&[token]);
            }
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Clear { .. } => {
            plan.clear();
            Reply::Settled {
                standing: standing(plan),
            }
        }

        Ask::Look { from, limit } => {
            // **From the head when the caller names nothing**, which is what a programme with
            // nothing playing shows. A token this programme no longer holds is the same case:
            // the alternative is an empty page, which a view cannot tell from an empty
            // programme.
            let at = from
                .map(EntryToken::from_u64)
                .and_then(|t| plan.position(t))
                .unwrap_or(0);
            let entries: Vec<Shown> = plan
                .entries()
                .iter()
                .skip(at)
                .take(limit)
                .map(|e| Shown {
                    token: e.token.as_u64(),
                    row: e.row,
                })
                .collect();
            Reply::Look {
                standing: standing(plan),
                at,
                next: plan
                    .entries()
                    .get(at + entries.len())
                    .map(|e| e.token.as_u64()),
                entries,
            }
        }
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
            let (encoding, locator) = library.open(recording, &[device], &[])?;
            let loudness = library.playable(entry.row).and_then(|p| p.gain);
            Some(Cued {
                token: entry.token.as_u64(),
                recording,
                encoding: encoding.id,
                // **The library's length, not the file's.** `Listed::duration` is what the
                // fold says; a row with no length yields zero, which `counts_as_played`
                // already treats as "use the four-minute floor".
                length: library
                    .describe(entry.row)
                    .map(|l| l.duration)
                    .unwrap_or_default(),
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

        let Reply::Window { cues, standing } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows: rows.clone(),
                window: 2,
                base: None,
            },
        ) else {
            panic!("a play did not answer with a window");
        };

        assert_eq!(cues.len(), 2, "the window was not the size asked for");
        assert!(
            standing.version > 1,
            "a play left the version where it started"
        );
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

        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        ) else {
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
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        );

        match ask(&mut plan, &l, Ask::After { token: 9_999, n: 2 }) {
            Reply::Stale { standing } => assert!(standing.version > 0),
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
                base: None,
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
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        );
        ask(&mut plan, &l, Ask::SetMode(Mode::RepeatAll));

        let Reply::Look {
            standing, entries, ..
        } = ask(
            &mut plan,
            &l,
            Ask::Look {
                from: None,
                limit: 2,
            },
        )
        else {
            panic!("a look did not answer with one");
        };
        assert_eq!(standing.mode, Mode::RepeatAll, "the mode was not reported");
        assert_eq!(entries.len(), 2, "the limit was not applied");
        assert_eq!(standing.total, 3, "a limited look misreported the length");
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
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 2,
                base: None,
            },
        ) else {
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
            base: None,
        }) else {
            panic!("the desk did not answer a Play");
        };
        assert_eq!(cues.len(), 2);
        let playing = cues[1].token;

        // A second job sees the same programme — the thread kept it.
        let Some(Reply::Look {
            entries, standing, ..
        }) = desk.programme(Ask::Look {
            from: None,
            limit: 10,
        })
        else {
            panic!("the desk did not answer a Look");
        };
        assert_eq!(
            standing.total, 3,
            "the programme did not survive the first job"
        );
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
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        ) else {
            panic!("no window");
        };
        let playing = cues[0].token;

        let held: Vec<u64> = plan.entries().iter().map(|e| e.token.as_u64()).collect();
        let reversed: Vec<u64> = held.iter().rev().copied().collect();
        ask(&mut plan, &l, Ask::Reorder { order: reversed });

        // Still valid — the token was carried, not re-minted.
        assert!(
            matches!(
                ask(
                    &mut plan,
                    &l,
                    Ask::Look {
                        from: None,
                        limit: 10
                    }
                ),
                Reply::Look {
                    standing: Standing { total: 3, .. },
                    ..
                }
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

    /// **`prev` walks the programme backwards, and wraps when repeating.**
    #[test]
    fn before_walks_back_and_repeat_all_wraps_to_the_end() {
        let (l, rows) = library(3);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        );
        let held: Vec<u64> = plan.entries().iter().map(|e| e.token.as_u64()).collect();

        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Before {
                token: held[1],
                n: 1,
            },
        ) else {
            panic!("before did not answer with a window");
        };
        assert_eq!(cues[0].token, held[0], "it did not step back one");

        // The first entry has nothing before it, until the programme repeats.
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Before {
                token: held[0],
                n: 1,
            },
        ) else {
            panic!("no window");
        };
        assert!(cues.is_empty(), "once-through walked back off the front");

        ask(&mut plan, &l, Ask::SetMode(Mode::RepeatAll));
        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Before {
                token: held[0],
                n: 1,
            },
        ) else {
            panic!("no window");
        };
        assert_eq!(cues[0].token, held[2], "repeat-all did not wrap backwards");
    }

    /// **A shuffle toggle keeps every token, so the track playing keeps playing.**
    #[test]
    fn shuffling_reorders_without_invalidating_the_position() {
        let (l, rows) = library(8);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Open {
                at: Some(rows[0]),
                scope: crate::next::Scope::All,
                shuffle: false,
                seed: 0,
                window: 1,
            },
        );
        let playing = plan.entries()[0].token;
        let sorted: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();

        ask(&mut plan, &l, Ask::Shuffle { on: true, seed: 99 });
        let after: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();

        assert_ne!(after, sorted, "a shuffle of eight rows changed nothing");
        assert_eq!(
            plan.entry(playing).map(|e| e.row),
            Some(sorted[0]),
            "the playing entry's token went stale across a shuffle"
        );
        assert!(plan.shuffled(), "the toggle was not recorded");

        ask(&mut plan, &l, Ask::Shuffle { on: false, seed: 0 });
        let back: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();
        assert_eq!(
            back, sorted,
            "turning shuffle off did not restore the order"
        );
        assert!(!plan.shuffled(), "the toggle was not recorded going back");
    }

    /// **The permutation is over the programme, not over the library.**
    ///
    /// This test first claimed a pool-derived order would *drop* a hand-placed entry, and
    /// two mutations killed nothing — because `reorder_by_row` sorts unmentioned entries to
    /// the end rather than discarding them, so membership is safe either way. The real
    /// difference is the order: a Fisher-Yates over 4 library rows is not the same
    /// permutation as one over the 3 a person is actually playing, and only the second is
    /// what "shuffle my queue" means.
    ///
    /// So it is asserted against `next::shuffled` over the held rows, which is the only
    /// form of the claim that can fail.
    #[test]
    fn a_shuffle_permutes_the_programme_and_not_the_library() {
        let (l, rows) = library(8);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows: rows[..5].to_vec(),
                window: 1,
                base: None,
            },
        );
        assert_eq!(
            plan.len(),
            5,
            "the programme is not a subset of the library"
        );

        ask(&mut plan, &l, Ask::Shuffle { on: true, seed: 31 });
        let got: Vec<CopyId> = plan.entries().iter().map(|e| e.row).collect();

        let members: std::collections::HashSet<CopyId> = rows[..5].iter().copied().collect();
        let shelf = crate::shelf::Shelf::from_members(&l, members, true);
        let expected = crate::next::shuffled(shelf.sorted(), 31);

        assert_eq!(
            got.len(),
            5,
            "the shuffle changed the programme's membership"
        );
        assert_eq!(
            got, expected,
            "the permutation was not over the five rows held"
        );
    }

    /// **A rescope anchors at the entry the caller names**, because only the caller knows
    /// which one is sounding.
    #[test]
    fn a_rescope_narrows_around_the_callers_token() {
        let (l, rows) = library(4);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Open {
                at: Some(rows[2]),
                scope: crate::next::Scope::All,
                shuffle: false,
                seed: 0,
                window: 1,
            },
        );
        let playing = plan
            .entries()
            .iter()
            .find(|e| e.row == rows[2])
            .unwrap()
            .token;

        let Reply::Window { cues, .. } = ask(
            &mut plan,
            &l,
            Ask::Rescope {
                at: Some(playing.as_u64()),
                scope: crate::next::Scope::Inner,
                shuffle: false,
                seed: 0,
                window: 1,
            },
        ) else {
            panic!("a rescope did not answer with a window");
        };

        // One album in the fixture, so `Inner` keeps all four — what is asserted is that
        // the anchor was resolved and the cue is the row that was playing, not the head.
        assert_eq!(plan.len(), 4);
        assert_eq!(
            cues.len(),
            1,
            "the rescope cued nothing, so the anchor did not resolve"
        );
        let cued = plan.entry(crate::programme::EntryToken::from_u64(cues[0].token));
        assert_eq!(
            cued.map(|e| e.row),
            Some(rows[2]),
            "the rescope cued from the head rather than the anchor"
        );
    }

    /// **A `Look` reports the toggles**, which is where a view draws them from now.
    ///
    /// §6 put `scope` in the engine "because it determines what plays next". It determines
    /// what the *programme* is, once — so the reason no longer holds and the state moved.
    #[test]
    fn a_look_reports_the_scope_and_the_shuffle() {
        let (l, rows) = library(3);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Open {
                at: Some(rows[0]),
                scope: crate::next::Scope::Outer,
                shuffle: true,
                seed: 5,
                window: 1,
            },
        );
        let Reply::Look { standing, .. } = ask(
            &mut plan,
            &l,
            Ask::Look {
                from: None,
                limit: 0,
            },
        ) else {
            panic!("no look");
        };
        assert_eq!(
            standing.scope,
            crate::next::Scope::Outer,
            "the scope was not recorded"
        );
        assert!(standing.shuffled, "the shuffle was not recorded");
    }

    /// **A look is a page, and the pages cover the programme exactly once.**
    ///
    /// The whole thing is never a useful answer: after a play gesture at scope `all` the
    /// programme *is* the library, and a pane drawing 7,271 rows is drawing the library rather
    /// than what plays next.
    ///
    /// Asserted by walking `next` to the end and comparing what came back to the programme —
    /// so an overlap or a gap fails, not just a wrong count.
    #[test]
    fn looks_page_through_the_programme_without_gaps_or_overlap() {
        let (l, rows) = library(9);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows: rows.clone(),
                window: 1,
                base: None,
            },
        );

        let mut seen: Vec<CopyId> = Vec::new();
        let mut from = None;
        let mut pages = 0;
        loop {
            let Reply::Look {
                entries,
                at,
                next,
                standing,
            } = ask(&mut plan, &l, Ask::Look { from, limit: 4 })
            else {
                panic!("a look did not answer with one");
            };
            assert_eq!(standing.total, 9, "the total is not the whole programme");
            assert_eq!(
                at,
                seen.len(),
                "the page did not start where the last ended"
            );
            seen.extend(entries.iter().map(|e| e.row));
            pages += 1;
            assert!(pages < 10, "paging did not terminate");
            match next {
                Some(token) => from = Some(token),
                None => break,
            }
        }

        assert_eq!(pages, 3, "nine entries in fours is three pages");
        assert_eq!(
            seen,
            plan.entries().iter().map(|e| e.row).collect::<Vec<_>>(),
            "the pages do not reassemble the programme"
        );
    }

    /// **A look anchored at a token starts there** — which is what makes the default "up
    /// next" rather than "the top of your library".
    #[test]
    fn a_look_from_a_token_starts_at_it() {
        let (l, rows) = library(6);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        );
        let fourth = plan.entries()[3];

        let Reply::Look { entries, at, .. } = ask(
            &mut plan,
            &l,
            Ask::Look {
                from: Some(fourth.token.as_u64()),
                limit: 2,
            },
        ) else {
            panic!("no look");
        };
        assert_eq!(at, 3, "the page did not report where it starts");
        assert_eq!(entries.len(), 2);
        assert_eq!(
            entries[0].token,
            fourth.token.as_u64(),
            "it did not start at the anchor"
        );
    }

    /// **A token the programme no longer holds pages from the head**, rather than answering
    /// with nothing.
    ///
    /// A view cannot tell an empty page from an empty programme, and "your queue is empty" is
    /// the wrong thing to draw over a library that is playing.
    #[test]
    fn a_look_from_a_stale_token_starts_over() {
        let (l, rows) = library(4);
        let mut plan = Programme::new();
        ask(
            &mut plan,
            &l,
            Ask::Play {
                rows,
                window: 1,
                base: None,
            },
        );
        let Reply::Look { entries, at, .. } = ask(
            &mut plan,
            &l,
            Ask::Look {
                from: Some(9_999),
                limit: 2,
            },
        ) else {
            panic!("no look");
        };
        assert_eq!(at, 0, "an unknown token did not page from the head");
        assert_eq!(entries.len(), 2, "it answered with nothing");
    }
}
