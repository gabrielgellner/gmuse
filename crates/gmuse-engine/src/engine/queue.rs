//! **The window, and the asks behind it** — #257, replacing #232's materialised pool.
//!
//! ## What this was, and why the replacement is smaller
//!
//! The decoder asks for the next track with `PlayerEvent::NeedNext` while the current one
//! is still playing, and the answer has to arrive before it ends or the gapless join is
//! missed. §4.4 forbids the control thread from running a query, so this file used to hold
//! a **materialised pool** — the whole library, ordered, rebuilt on every published fold —
//! and walk it by scope and shuffle.
//!
//! That worked and cost a fold in the engine, which is what #257 is about. A window is the
//! better answer to the same problem: **`NeedNext` is a `pop_front` with no ask at all**,
//! and the refill happens afterwards, off the deadline.
//!
//! ## What blocks and what does not
//!
//! - **`NeedNext` never asks.** It takes from the window. If the window is empty the answer
//!   is "stop", which is an answer and has to be one — the decoder waits, and
//!   `NEXT_REPLY_TIMEOUT` is what happens if none comes.
//! - **A refill never blocks.** `Desk::programme_later` hands back a receiver and the
//!   control loop polls it. Jobs are FIFO, so a refill queued behind a scan of 7,267 files
//!   would wait seconds, and I16 keeps the librarian off this thread precisely so a long
//!   job cannot stall transport.
//! - **A gesture blocks, and that is a measured defect rather than a trade.** `prev`, a mode
//!   change, a scope change and the socket's `queue.*` all wait on the librarian. The round
//!   trip is microseconds warm — and **9.45s on the first gesture after a launch**, because
//!   a launch reconciles 7,267 files and jobs are served FIFO. **#259** has the measurement
//!   and the options; see `PATIENCE`.
//!
//! ## What it decides: nothing
//!
//! No pool, no order, no mode. Shuffle and repeat are rules the librarian answers, so a
//! refill under repeat-current simply returns the same entry again and nothing here can
//! tell. There is no mode to set — which is the decision working rather than a gap.

use std::sync::Arc;

use gmuse_library::librarian::Desk;
use gmuse_library::librarian::programme::{Ask, Cued, Reply};
use gmuse_library::next::Scope;
use gmuse_library::programme::Mode as Rule;

use crate::instruction::{Mode, Switch};

use super::window::{Cue, Window};

/// How long a gesture waits on the librarian before giving up.
///
/// **Measured, and no value here is right — see the note below.** On the real library the
/// first `player.load` after a launch takes **9.45s** and the second takes **0.41s**,
/// because a launch reconciles 7,267 files and the librarian serves jobs FIFO: the play
/// gesture queues behind the scan. A 2s budget makes that gesture silently do nothing; a
/// budget long enough to survive it stalls transport for nine seconds.
///
/// So the bound is not the fix, and a gesture blocking the control thread at all is the
/// defect. The refill already has the right shape — `programme_later`, polled on a later
/// tick — and the gestures need it too. **#259.**
const PATIENCE: std::time::Duration = std::time::Duration::from_secs(2);

/// The cues in hand, and the door to the programme behind them.
pub struct Playing {
    window: Window,
    /// A refill that has been asked for and not yet absorbed.
    refill: Option<crossbeam_channel::Receiver<Reply>>,
    /// Seeds the next reshuffle. **Bumped per reshuffle so two are different orders**, and
    /// held here because a seed is not a library fact.
    seed: u64,
}

impl Playing {
    pub fn new() -> Self {
        Self {
            window: Window::new(),
            refill: None,
            seed: 0x5EED,
        }
    }

    /// **The answer to `NeedNext`** — a `pop_front`, and no ask.
    pub fn take_next(&mut self) -> Option<Cue> {
        self.window.take()
    }

    /// What is cued after the current one, without consuming it.
    pub fn peek(&self) -> Option<&Cue> {
        self.window.peek()
    }

    /// The programme version the window came from.
    pub fn version(&self) -> u64 {
        self.window.version()
    }

    /// **Top the window up, without waiting for the answer.**
    ///
    /// Called every tick of the control loop. Does nothing while an ask is outstanding or
    /// the window is long enough, so it is a load and a comparison in the common case.
    pub fn keep_topped_up(&mut self, desk: &Arc<Desk>) {
        // Absorb an answer that has arrived.
        if let Some(rx) = &self.refill {
            match rx.try_recv() {
                Ok(Reply::Window { version, cues }) => {
                    self.window.extend(version, cues.into_iter().map(cue_of));
                    self.refill = None;
                }
                // **The position is gone, so take the top of the programme.** A view
                // replaced the queue while this was in flight; playing on from a token
                // that no longer exists is not an option, and asking again from the same
                // token would loop.
                Ok(Reply::Stale { .. }) => {
                    self.refill = None;
                    self.window.answered();
                    self.reopen(desk);
                }
                Ok(_) => {
                    // A `Settled` or `Look` cannot arrive here: this receiver was created
                    // for an `After`. Absorbed rather than ignored so the slot frees.
                    self.refill = None;
                    self.window.answered();
                }
                Err(crossbeam_channel::TryRecvError::Empty) => return,
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.refill = None;
                    self.window.answered();
                }
            }
        }

        if !self.window.wants_more() {
            return;
        }
        // **From the end of what is held**, not from the playing position — see
        // `Window::tail`. Addressing a refill from `from` re-fetches entries already in hand
        // and the queue walks two tracks for ever.
        let Some(from) = self.window.tail() else {
            return;
        };
        let n = self.window.short_by();
        if let Some(rx) = desk.programme_later(Ask::After { token: from, n }) {
            self.window.asked();
            self.refill = Some(rx);
        }
    }

    /// **Throw the window away and cue the start of the programme as it stands.**
    ///
    /// For a `Stale` refill and for a view that replaced the queue. `Ask::Head` rather than
    /// `Ask::Open`, and the difference is the whole meaning of a resync: **the programme is
    /// right and this side's copy of it is not**, so nothing may be rebuilt.
    pub fn reopen(&mut self, desk: &Arc<Desk>) {
        let Some(Reply::Window { version, cues }) = ask(desk, Ask::Head { n: 3 }) else {
            return;
        };
        self.window.replace(version, cues.into_iter().map(cue_of));
    }

    /// **Open a programme over the library and cue from one row** — a play gesture.
    pub fn open(
        &mut self,
        desk: &Arc<Desk>,
        at: Option<gmuse_ids::copy::CopyId>,
        scope: Scope,
        shuffle: bool,
        window: usize,
    ) -> Option<Cue> {
        let reply = ask(
            desk,
            Ask::Open {
                at,
                scope,
                shuffle,
                seed: self.seed,
                window,
            },
        )?;
        let Reply::Window { version, cues } = reply else {
            return None;
        };
        self.window.replace(version, cues.into_iter().map(cue_of));
        self.window.take()
    }

    /// **Play this recording, opening a programme around it** — for `Ask::Load`.
    ///
    /// The window is replaced, because the programme was: what follows the picked track is
    /// whatever the new programme says, which is the point of picking one.
    pub fn start(
        &mut self,
        desk: &Arc<Desk>,
        recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
    ) -> Option<Cue> {
        let Reply::Window { version, cues } = ask(
            desk,
            Ask::Start {
                recording,
                window: 3,
            },
        )?
        else {
            return None;
        };
        self.window.replace(version, cues.into_iter().map(cue_of));
        self.window.take()
    }

    /// **The same, without a `Playing` to hold the window** — the session resume.
    ///
    /// Runs inside the device callback, where `Held` has not been built yet, so it can only
    /// answer "what to load". The window it opened is picked up by the first `reopen`.
    pub fn resume(
        desk: &Arc<Desk>,
        recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
    ) -> Option<Cue> {
        let Reply::Window { cues, .. } = ask(
            desk,
            Ask::Start {
                recording,
                window: 1,
            },
        )?
        else {
            return None;
        };
        cues.into_iter().next().map(cue_of)
    }

    /// **Open a programme over the whole library, if there is not one yet.**
    ///
    /// The old design had an *implicit* programme: the pool was the whole library, always, and
    /// a cursor walked it. So `next` with nothing playing played the first row and `prev`
    /// played the last, and two integration tests assert exactly that. With an explicit
    /// programme those gestures have nothing to walk until one exists.
    ///
    /// **Lazy rather than at startup, and the reason is a question this does not settle.**
    /// Opening one eagerly makes `queue.get` return 7,271 rows, because a programme *is* the
    /// play order and the play order is the library. That is arguably right — "up next" is
    /// then well-defined — but the wire's `queue.get` has always meant "what I queued", and
    /// two tests say so. Deferring the open keeps that meaning until something plays, and
    /// then it changes. **Which of the two `queue.get` should mean is a protocol decision,
    /// not a refactor** — #257.
    fn open_library_if_empty(&mut self, desk: &Arc<Desk>) {
        let empty = matches!(
            ask(desk, Ask::Look { limit: 0 }),
            Some(Reply::Look { total: 0, .. })
        );
        if !empty {
            return;
        }
        let _ = ask(
            desk,
            Ask::Open {
                at: None,
                scope: Scope::All,
                shuffle: false,
                seed: self.seed,
                window: 0,
            },
        );
    }

    /// **Skip forward as a gesture**, which may ask where `NeedNext` may not.
    ///
    /// Distinct from [`take_next`] on purpose. `NeedNext` is on the decoder's deadline and
    /// must answer from the window or not at all; a person pressing a key is waiting anyway,
    /// and with nothing playing the answer is the head of the programme rather than nothing.
    ///
    /// [`take_next`]: Playing::take_next
    pub fn skip_next(&mut self, desk: &Arc<Desk>) -> Option<Cue> {
        if let Some(cue) = self.window.take() {
            return Some(cue);
        }
        self.open_library_if_empty(desk);
        let Reply::Window { version, cues } = ask(desk, Ask::Head { n: 3 })? else {
            return None;
        };
        self.window.replace(version, cues.into_iter().map(cue_of));
        self.window.take()
    }

    /// **The previous entry, loaded rather than spliced.**
    ///
    /// With nothing playing it is the *last* entry, which is what `next::step` did when it
    /// had no current track: start at whichever end we are walking from.
    pub fn take_prev(&mut self, desk: &Arc<Desk>) -> Option<Cue> {
        let reply = match self.window.from() {
            Some(from) => ask(desk, Ask::Before { token: from, n: 1 })?,
            None => {
                self.open_library_if_empty(desk);
                ask(desk, Ask::Tail { n: 1 })?
            }
        };
        let Reply::Window { version, cues } = reply else {
            return None;
        };
        let cue = cues.into_iter().next().map(cue_of)?;
        // **The position moves backwards too**, or the next refill would carry on from where
        // we *were* and the track after `prev` would be the one we just left.
        self.window.replace(version, std::iter::empty());
        self.window.resume_at(cue.token);
        Some(cue)
    }

    /// **A mode change**, forwarded and then resynced.
    ///
    /// Resynced because the mode changes what follows: the window was filled under the old
    /// rule and its second entry may now be wrong.
    pub fn set_mode(&mut self, desk: &Arc<Desk>, which: Mode, to: Switch) -> u64 {
        let Some(Reply::Look { mode, shuffled, .. }) = ask(desk, Ask::Look { limit: 0 }) else {
            return self.window.version();
        };
        let on = |now: bool| match to {
            Switch::On => true,
            Switch::Off => false,
            Switch::Flip => !now,
        };

        let settled = match which {
            Mode::Shuffle => ask(
                desk,
                Ask::Shuffle {
                    on: on(shuffled),
                    seed: self.next_seed(),
                },
            ),
            Mode::Repeat => ask(
                desk,
                Ask::SetMode(if on(mode == Rule::RepeatAll) {
                    Rule::RepeatAll
                } else {
                    Rule::OnceThrough
                }),
            ),
            Mode::RepeatCurrent => ask(
                desk,
                Ask::SetMode(if on(mode == Rule::RepeatCurrent) {
                    Rule::RepeatCurrent
                } else {
                    Rule::OnceThrough
                }),
            ),
        };

        let version = match settled {
            Some(Reply::Settled { version }) => version,
            _ => self.window.version(),
        };
        self.resync_after(desk, version);
        version
    }

    /// **A scope change**, which rebuilds the pool around what is sounding.
    pub fn set_scope(&mut self, desk: &Arc<Desk>, scope: Scope) -> u64 {
        let at = self.window.from();
        let Some(Reply::Window { version, cues }) = ask(
            desk,
            Ask::Rescope {
                at,
                scope,
                shuffle: self.shuffled(desk),
                seed: self.seed,
                window: 3,
            },
        ) else {
            return self.window.version();
        };
        // **The first cue is what is already sounding**, so it is dropped rather than
        // queued to play again: a scope change is not a restart.
        let mut cues = cues.into_iter();
        if at.is_some() {
            cues.next();
        }
        self.window.replace(version, cues.map(cue_of));
        version
    }

    /// The four fields the `queue` event carries (#233), read from the librarian.
    pub fn modes(&self, desk: &Arc<Desk>) -> (Scope, bool, bool, bool) {
        match ask(desk, Ask::Look { limit: 0 }) {
            Some(Reply::Look {
                mode,
                scope,
                shuffled,
                ..
            }) => (
                scope,
                shuffled,
                mode == Rule::RepeatAll,
                mode == Rule::RepeatCurrent,
            ),
            _ => Default::default(),
        }
    }

    /// The narrowing in force, read from the librarian.
    pub fn scope(&self, desk: &Arc<Desk>) -> Scope {
        match ask(desk, Ask::Look { limit: 0 }) {
            Some(Reply::Look { scope, .. }) => scope,
            _ => Scope::default(),
        }
    }

    fn shuffled(&self, desk: &Arc<Desk>) -> bool {
        matches!(ask(desk, Ask::Look { limit: 0 }), Some(Reply::Look { shuffled, .. }) if shuffled)
    }

    /// Refill from the current position under the new rule, discarding what was held.
    fn resync_after(&mut self, desk: &Arc<Desk>, version: u64) {
        let Some(from) = self.window.from() else {
            return;
        };
        if let Some(Reply::Window { cues, .. }) = ask(desk, Ask::After { token: from, n: 3 }) {
            self.window.replace(version, cues.into_iter().map(cue_of));
            // `replace` clears `from`, and the position has not actually moved.
            self.window.resume_at(from);
        }
    }

    fn next_seed(&mut self) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        self.seed
    }
}

impl Default for Playing {
    fn default() -> Self {
        Self::new()
    }
}

/// One bounded round trip to the librarian.
fn ask(desk: &Arc<Desk>, what: Ask) -> Option<Reply> {
    desk.programme_later(what)?.recv_timeout(PATIENCE).ok()
}

/// The librarian's cue, as this side holds it.
///
/// A separate type on purpose: it is what `gmuse-engine` will name when the `Desk` is
/// replaced by a channel and this crate stops depending on `gmuse-library` at all (#257).
fn cue_of(cued: Cued) -> Cue {
    Cue {
        token: cued.token,
        recording: cued.recording,
        path: cued.path,
        track_db: cued.track_db,
        track_peak: cued.track_peak,
        album_db: cued.album_db,
        album_peak: cued.album_peak,
    }
}
