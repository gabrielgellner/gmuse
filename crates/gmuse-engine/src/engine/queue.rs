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
//! - **A gesture never blocks either** — #259. It goes on a line with an [`Awaiting`] saying
//!   what its answer means, and a later tick absorbs the reply and acts. So a play gesture
//!   arriving while the librarian is reconciling 7,267 files is **late, not lost**; before,
//!   it answered `accepted: true` and did nothing.
//!
//!   The one exception is `cue_for_recovery`, which runs on the device-recovery thread where
//!   nothing is playing and nothing has a deadline.
//!
//!   **The latency behind a launch scan is #260**, not this: a bare `Look` costs 9.26s on the
//!   first call too, because the scan walks the disk on the librarian thread.
//!
//! ## What it decides: nothing
//!
//! No pool, no order, no mode. Shuffle and repeat are rules the librarian answers, so a
//! refill under repeat-current simply returns the same entry again and nothing here can
//! tell. There is no mode to set — which is the decision working rather than a gap.

use std::sync::Arc;

use gmuse_library::librarian::Desk;
use gmuse_library::librarian::programme::{Ask, Cued, Reply, Standing};
use gmuse_library::next::Scope;
use gmuse_library::programme::Mode as Rule;

use crate::instruction::{Mode, Switch};

use super::window::{Cue, KEEP, Window};

/// **What an outstanding ask was for, and therefore what to do with its answer** — #259.
///
/// The control thread never waits on the librarian. A gesture puts an ask on the pipeline
/// and says here what the answer means; a later tick absorbs it and acts. So a play gesture
/// arriving while the librarian is reconciling 7,267 files is *late*, not *lost* — which is
/// the difference the 9.45s measurement made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Awaiting {
    /// Extend the window. What a refill is.
    Refill,
    /// Replace the window and load its first cue — a play gesture, or `next` off the end.
    LoadFirst,
    /// Replace the window and load the *only* cue, which is behind the position — `prev`.
    LoadBack,
    /// Replace the window, load nothing. A view changed the programme under us.
    Resync,
    /// Replace the window but keep the position: the cues changed, the playing entry did not.
    ResyncAt(u64),
    /// **Then ask what follows the position**, because a mode change made the window wrong
    /// rather than short. The second step of a two-step gesture.
    ThenResync,
    /// Nothing to do but free the slot and refresh `standing`.
    Noted,
}

/// How many asks may wait to be sent before one is dropped.
///
/// **Four, and a drop is reported rather than silent.** Gestures are human-paced and the
/// window answers `next` without asking at all, so a backlog means the librarian is busy;
/// growing the queue without bound would turn one slow scan into thousands of queued jobs.
const BACKLOG: usize = 4;

/// The cues in hand, the programme's standing state, and the asks in flight.
pub struct Playing {
    window: Window,
    /// **What the programme says about itself, cached.** Refreshed by every reply, so
    /// nothing has to *ask* for the version or the toggles — an ask not sent is an ask that
    /// cannot block (#259).
    standing: Standing,
    /// Asks not yet sent, oldest first.
    queued: std::collections::VecDeque<(Ask, Awaiting)>,
    /// The one ask in flight. **One at a time**, so replies cannot arrive out of order and
    /// a later gesture cannot overtake the refill it depends on.
    flight: Option<(crossbeam_channel::Receiver<Reply>, Awaiting)>,
    /// Seeds the next reshuffle, bumped per reshuffle so two are different orders.
    seed: u64,
}

impl Playing {
    pub fn new() -> Self {
        Self {
            window: Window::new(),
            standing: Standing::default(),
            queued: std::collections::VecDeque::new(),
            flight: None,
            seed: 0x5EED,
        }
    }

    /// **The answer to `NeedNext`** — a `pop_front`, and no ask.
    pub fn take_next(&mut self) -> Option<Cue> {
        self.window.take()
    }

    pub fn peek(&self) -> Option<&Cue> {
        self.window.peek()
    }

    /// The programme version, from the cache. **No round trip.**
    pub fn version(&self) -> u64 {
        self.standing.version
    }

    /// The four fields the `queue` event carries (#233), from the cache.
    ///
    /// **Read rather than asked**, and that matters: this is on the publish path, so an ask
    /// here put a librarian round trip in the control loop every time the version moved.
    pub fn modes(&self) -> (Scope, bool, bool, bool) {
        (
            self.standing.scope,
            self.standing.shuffled,
            self.standing.mode == Rule::RepeatAll,
            self.standing.mode == Rule::RepeatCurrent,
        )
    }

    pub fn scope(&self) -> Scope {
        self.standing.scope
    }

    /// **One tick of the pipeline.** Absorb a reply if one has arrived, then send the next
    /// ask if the line is free, then top the window up if it is short.
    ///
    /// Returns the cue to load, if the answer that arrived was a gesture's. The caller loads
    /// it, because loading is the player's business and this file's is the programme.
    pub fn tick(&mut self, desk: &Arc<Desk>) -> Option<Cue> {
        let loaded = self.absorb();
        self.send_next(desk);
        self.top_up();
        loaded
    }

    /// Take the reply if there is one, and act on it.
    fn absorb(&mut self) -> Option<Cue> {
        let (rx, what) = self.flight.as_ref()?;
        let what = *what;
        let reply = match rx.try_recv() {
            Ok(reply) => reply,
            Err(crossbeam_channel::TryRecvError::Empty) => return None,
            // The librarian is gone. Free the line; the next tick may try again.
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                self.flight = None;
                self.window.answered();
                return None;
            }
        };
        self.flight = None;

        // **Every reply refreshes the cache**, whatever it was asked for.
        if let Some(standing) = standing_of(&reply) {
            self.standing = standing;
        }

        let cues: Vec<Cue> = match reply {
            Reply::Window { cues, .. } => cues.into_iter().map(cue_of).collect(),
            // The position is gone — a view replaced the programme. Take the top of it
            // rather than playing on from a token that no longer exists.
            Reply::Stale { .. } => {
                self.window.answered();
                self.push(Ask::Head { n: KEEP }, Awaiting::Resync);
                return None;
            }
            Reply::Settled { .. } | Reply::Look { .. } => {
                self.window.answered();
                if what == Awaiting::ThenResync {
                    // A mode change landed; now the window has to reflect it.
                    match self.window.from() {
                        Some(at) => {
                            self.push(Ask::After { token: at, n: KEEP }, Awaiting::ResyncAt(at))
                        }
                        None => self.push(Ask::Head { n: KEEP }, Awaiting::Resync),
                    }
                }
                return None;
            }
        };

        let version = self.standing.version;
        match what {
            Awaiting::Refill => {
                self.window.extend(version, cues);
                None
            }
            Awaiting::LoadFirst => {
                self.window.replace(version, cues);
                self.window.take()
            }
            Awaiting::LoadBack => {
                let back = cues.first().cloned();
                self.window.replace(version, cues);
                // **The position moves backwards too**, or the next refill carries on from
                // where we *were* and the track after `prev` is the one we just left.
                if let Some(cue) = &back {
                    self.window.resume_at(cue.token);
                }
                self.window.take()
            }
            Awaiting::Resync => {
                self.window.replace(version, cues);
                None
            }
            Awaiting::ResyncAt(at) => {
                self.window.replace(version, cues);
                self.window.resume_at(at);
                None
            }
            // A `Window` is not what these were for; the cache is refreshed either way.
            Awaiting::ThenResync | Awaiting::Noted => None,
        }
    }

    /// Send the oldest queued ask, if the line is free.
    fn send_next(&mut self, desk: &Arc<Desk>) {
        if self.flight.is_some() {
            return;
        }
        let Some((ask, what)) = self.queued.pop_front() else {
            return;
        };
        match desk.programme_later(ask) {
            Some(rx) => self.flight = Some((rx, what)),
            // The librarian is gone; nothing to do and nothing to wait for.
            None => self.window.answered(),
        }
    }

    /// Ask for more when the window is short and nothing is already on the way.
    fn top_up(&mut self) {
        if !self.window.wants_more() {
            return;
        }
        if self.pending(Awaiting::Refill) {
            return;
        }
        // **From the end of what is held**, not from the playing position — see
        // `Window::tail`. Addressing a refill from `from` re-fetches entries already in
        // hand and the queue walks two tracks for ever.
        let Some(from) = self.window.tail() else {
            return;
        };
        let n = self.window.short_by();
        self.window.asked();
        self.push(Ask::After { token: from, n }, Awaiting::Refill);
    }

    fn pending(&self, what: Awaiting) -> bool {
        self.flight.as_ref().is_some_and(|(_, w)| *w == what)
            || self.queued.iter().any(|(_, w)| *w == what)
    }

    /// Put an ask on the line.
    ///
    /// **Drops the oldest when full, and says so.** A backlog means the librarian is busy;
    /// an unbounded queue would turn one slow scan into thousands of jobs, and a silent drop
    /// would be a gesture that did nothing for no stated reason.
    fn push(&mut self, ask: Ask, what: Awaiting) {
        if self.queued.len() >= BACKLOG {
            let dropped = self.queued.pop_front();
            eprintln!(
                "gmuse engine: the librarian is behind, so a queued {:?} was dropped",
                dropped.map(|(_, w)| w)
            );
        }
        self.queued.push_back((ask, what));
    }

    /// **Where one recording's audio is, waiting for the answer** — device recovery only.
    ///
    /// The one place a blocking ask is right, and it is not the control loop: this runs on
    /// the recovery thread after a `DeviceLost`, which is already sitting in
    /// `wait_for_a_device` polling for hardware. Nothing is playing, nothing is waiting on a
    /// deadline, and the alternative is not restoring the session at all.
    ///
    /// Everything else goes through the pipeline, because on the control thread a wait is a
    /// transport stall — see the module header and #259.
    pub fn cue_for_recovery(
        desk: &Arc<Desk>,
        recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>,
    ) -> Option<Cue> {
        let rx = desk.programme_later(Ask::Start {
            recording,
            window: 1,
        })?;
        match rx.recv_timeout(std::time::Duration::from_secs(30)) {
            Ok(Reply::Window { cues, .. }) => cues.into_iter().next().map(cue_of),
            _ => None,
        }
    }

    /// **Play this recording, opening a programme around it.** Acts on a later tick.
    pub fn start(&mut self, recording: gmuse_ids::id::Id<gmuse_ids::entity::Recording>) {
        self.push(
            Ask::Start {
                recording,
                window: KEEP,
            },
            Awaiting::LoadFirst,
        );
    }

    /// **Skip forward.** From the window if it has one, else asked for.
    ///
    /// Returns the cue immediately in the common case, which is what makes `next` feel
    /// instant: the window already holds it and nothing is asked at all.
    pub fn skip_next(&mut self) -> Option<Cue> {
        if let Some(cue) = self.window.take() {
            return Some(cue);
        }
        if self.standing.total == 0 {
            // **The whole library, which is what the old design's pool implicitly was.** A
            // cursor walked it, so `next` with nothing playing played the first row. With an
            // explicit programme there is nothing to walk until one exists.
            self.push(self.over_the_library(KEEP), Awaiting::LoadFirst);
        } else {
            self.push(Ask::Head { n: KEEP }, Awaiting::LoadFirst);
        }
        None
    }

    /// A programme over the whole library, in the narrowing and order already in force.
    fn over_the_library(&self, window: usize) -> Ask {
        Ask::Open {
            at: None,
            scope: self.standing.scope,
            shuffle: self.standing.shuffled,
            seed: self.seed,
            window,
        }
    }

    /// **Go back.** With nothing playing that is the *last* entry, which is what
    /// `next::step` did with no current track: start at whichever end we walk from.
    pub fn skip_prev(&mut self) {
        if let Some(from) = self.window.from() {
            self.push(Ask::Before { token: from, n: 1 }, Awaiting::LoadBack);
            return;
        }
        // **Two asks, in order**, because the line is FIFO: build a programme if there is
        // none, then take its last entry. `next::step` did the same with no current track —
        // start at whichever end we are walking from.
        if self.standing.total == 0 {
            self.push(self.over_the_library(0), Awaiting::Noted);
        }
        self.push(Ask::Tail { n: 1 }, Awaiting::LoadBack);
    }

    /// **A mode change**, then a resync — the window was filled under the old rule.
    pub fn set_mode(&mut self, which: Mode, to: Switch) -> u64 {
        let on = |now: bool| match to {
            Switch::On => true,
            Switch::Off => false,
            Switch::Flip => !now,
        };
        let ask = match which {
            Mode::Shuffle => Ask::Shuffle {
                on: on(self.standing.shuffled),
                seed: self.next_seed(),
            },
            Mode::Repeat => Ask::SetMode(if on(self.standing.mode == Rule::RepeatAll) {
                Rule::RepeatAll
            } else {
                Rule::OnceThrough
            }),
            Mode::RepeatCurrent => Ask::SetMode(if on(self.standing.mode == Rule::RepeatCurrent) {
                Rule::RepeatCurrent
            } else {
                Rule::OnceThrough
            }),
        };
        self.push(ask, Awaiting::ThenResync);
        self.standing.version
    }

    /// **A scope change**, which rebuilds the pool around what is sounding.
    ///
    /// `ResyncAt` rather than `LoadFirst`: a narrowing is not a restart, so what is playing
    /// keeps playing and only what follows it changes.
    pub fn set_scope(&mut self, scope: Scope) -> u64 {
        let at = self.window.from();
        self.push(
            Ask::Rescope {
                at,
                scope,
                shuffle: self.standing.shuffled,
                seed: self.seed,
                window: KEEP,
            },
            match at {
                Some(token) => Awaiting::ResyncAt(token),
                None => Awaiting::Resync,
            },
        );
        self.standing.version
    }

    /// **A view changed the programme; take a fresh window.**
    ///
    /// `Ask::Head` rather than `Ask::Open`, and the difference is the whole meaning of a
    /// resync: the programme is right and this side's copy of it is not, so nothing may be
    /// rebuilt.
    pub fn resync(&mut self) {
        self.push(Ask::Head { n: KEEP }, Awaiting::Resync);
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

/// The standing state any reply carries.
fn standing_of(reply: &Reply) -> Option<Standing> {
    match reply {
        Reply::Window { standing, .. }
        | Reply::Stale { standing }
        | Reply::Look { standing, .. }
        | Reply::Settled { standing } => Some(*standing),
    }
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
