//! The clock behind "last wins" — issue #67, `domain-model.md` §4.
//!
//! "Last wins" is not a rule until *last* is defined, and a wall clock does not
//! define it. A laptop and a NAS drift, and a machine whose clock runs fast
//! wins every conflict for as long as the skew lasts — silently, because
//! nothing about the losing write looks wrong.
//!
//! `library-sync-design.md` §5 is blunt about the failure: "a device offline
//! for six months with a drifting RTC will produce timestamps that interleave
//! wrongly with everything else."
//!
//! ## The order
//!
//! 1. **causal order** where it is determinable
//! 2. a **hybrid logical clock** where two writes are genuinely concurrent
//! 3. **device id** as the final tiebreak, so the order is total
//!
//! A [`Stamp`] encodes all three at once: it sorts by `(wall, counter,
//! device)`, and the counter is what carries causality that physical time
//! cannot.
//!
//! ## Why a hybrid, and not either half
//!
//! A pure logical clock (Lamport) orders correctly and drifts away from real
//! time, so "what did I play last Tuesday" becomes unanswerable. A pure wall
//! clock reads well and orders wrongly. A hybrid keeps physical time as the
//! leading term and repairs it with a counter, so a stamp is both sortable and
//! still recognisably a time.
//!
//! The rule that makes it work is [`HybridClock::observe`]: reading another
//! device's event advances this device's clock past it. Everything written
//! afterwards therefore sorts after what was read, which is what "causal order
//! where determinable" means in practice — and it is the step a bare
//! `(timestamp, counter)` pair is missing.
//!
//! ## What this is not
//!
//! An HLC does not detect concurrency; it orders it. Two devices editing the
//! same field while disconnected still produce a conflict, and §4 requires that
//! be **surfaced** rather than resolved by whoever sorted higher. This clock
//! decides which value is shown; it never decides that the disagreement did not
//! happen.

use std::cmp::Ordering;

use crate::model::entity::Device;
use crate::model::id::Id;

/// How far ahead of us a remote stamp may be before it is worth complaining
/// about — five minutes, in milliseconds.
///
/// Causality is still honoured beyond it: we advance anyway, because refusing
/// would put our later events *before* the ones that caused them. The bound is
/// a **detector**, not a limiter, in keeping with §4 — a machine with its clock
/// set to next year is a thing to be told about, not to be quietly corrected.
pub const IMPLAUSIBLE_DRIFT_MS: i64 = 5 * 60 * 1000;

/// When something happened, in an order every device agrees on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Stamp {
    /// Milliseconds since the epoch — the leading term, so a stamp still reads
    /// as a time.
    pub wall: i64,
    /// Ticks within one millisecond, and the repair for a clock that went
    /// backwards or stood still.
    pub counter: u32,
    /// The final tiebreak, so two devices cannot produce an equal stamp.
    pub device: Id<Device>,
}

impl Ord for Stamp {
    fn cmp(&self, other: &Self) -> Ordering {
        self.wall
            .cmp(&other.wall)
            .then(self.counter.cmp(&other.counter))
            .then(self.device.cmp(&other.device))
    }
}

impl PartialOrd for Stamp {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// One device's clock.
///
/// Physical time is passed in rather than read, so the ordering rules are
/// testable without waiting for a real millisecond to pass — and so a caller
/// that has a better idea of "now" than the system clock can say so.
#[derive(Debug, Clone)]
pub struct HybridClock {
    device: Id<Device>,
    wall: i64,
    counter: u32,
}

impl HybridClock {
    pub fn new(device: Id<Device>) -> Self {
        HybridClock {
            device,
            wall: 0,
            counter: 0,
        }
    }

    /// Stamp something that happened here, now.
    ///
    /// Never goes backwards, even if the system clock does — an NTP correction
    /// or a suspend-and-resume must not let a later event sort before an
    /// earlier one.
    pub fn stamp(&mut self, physical_ms: i64) -> Stamp {
        if physical_ms > self.wall {
            self.wall = physical_ms;
            self.counter = 0;
        } else {
            self.counter = self.counter.saturating_add(1);
        }
        Stamp {
            wall: self.wall,
            counter: self.counter,
            device: self.device,
        }
    }

    /// Take in what another device recorded, before writing anything that
    /// follows from having read it.
    ///
    /// This is the step that makes the order causal: afterwards, everything
    /// this device stamps sorts after `remote`.
    ///
    /// Returns [`Drift::Implausible`] when the remote stamp is further ahead of
    /// physical time than [`IMPLAUSIBLE_DRIFT_MS`]. The clock still advances —
    /// causality is not negotiable — but the caller is told, because a machine
    /// whose clock is set wrong will otherwise win every conflict for as long
    /// as the error lasts.
    pub fn observe(&mut self, remote: &Stamp, physical_ms: i64) -> Drift {
        let drift = remote.wall - physical_ms;

        let wall = physical_ms.max(self.wall).max(remote.wall);
        self.counter = if wall == self.wall && wall == remote.wall {
            self.counter.max(remote.counter).saturating_add(1)
        } else if wall == self.wall {
            self.counter.saturating_add(1)
        } else if wall == remote.wall {
            remote.counter.saturating_add(1)
        } else {
            0
        };
        self.wall = wall;

        if drift > IMPLAUSIBLE_DRIFT_MS {
            Drift::Implausible { ahead_ms: drift }
        } else {
            Drift::Fine
        }
    }

    pub fn device(&self) -> Id<Device> {
        self.device
    }
}

/// Whether a stamp we took in looked like it came from a working clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drift {
    Fine,
    /// Far enough ahead that someone should look at that machine's clock.
    Implausible {
        ahead_ms: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    /// The failure this whole module exists to prevent: a machine whose clock
    /// runs fast must not win a conflict it did not cause. It still sorts
    /// higher — that is what a hybrid clock is — but the caller is told.
    #[test]
    fn a_wildly_fast_remote_clock_is_reported() {
        let mut clock = HybridClock::new(dev(1));
        let remote = Stamp {
            wall: 1_000_000 + IMPLAUSIBLE_DRIFT_MS + 1,
            counter: 0,
            device: dev(2),
        };
        assert_eq!(
            clock.observe(&remote, 1_000_000),
            Drift::Implausible {
                ahead_ms: IMPLAUSIBLE_DRIFT_MS + 1
            }
        );
        assert!(
            clock.stamp(1_000_000) > remote,
            "causality is still honoured: we advanced past it"
        );
    }

    /// **The step a plain timestamp pair is missing.** Reading another
    /// device's event puts everything written afterwards after it.
    #[test]
    fn reading_an_event_orders_everything_after_it() {
        let mut clock = HybridClock::new(dev(1));
        let earlier = clock.stamp(500);
        let remote = Stamp {
            wall: 900,
            counter: 4,
            device: dev(2),
        };

        clock.observe(&remote, 600);
        let after = clock.stamp(600);

        assert!(earlier < remote, "it happened after ours");
        assert!(after > remote, "and ours now follows what we read");
    }

    /// A clock that jumps backwards — NTP, or a resume — must not reorder
    /// what has already happened.
    #[test]
    fn a_clock_going_backwards_does_not_reorder_anything() {
        let mut clock = HybridClock::new(dev(1));
        let first = clock.stamp(10_000);
        let second = clock.stamp(9_000); // the system clock stepped back
        let third = clock.stamp(9_001);

        assert!(first < second && second < third);
        assert_eq!(second.wall, first.wall, "wall time never retreats");
        assert_eq!(second.counter, 1, "the counter carries the order instead");
    }

    /// Many events inside one millisecond stay strictly ordered.
    #[test]
    fn events_in_the_same_millisecond_are_still_ordered() {
        let mut clock = HybridClock::new(dev(1));
        let stamps: Vec<_> = (0..5).map(|_| clock.stamp(42)).collect();
        assert!(stamps.windows(2).all(|w| w[0] < w[1]));
        assert!(stamps.iter().all(|s| s.wall == 42));
    }

    /// Two devices, same instant, same counter: the order is still total, and
    /// it is the same total order on both machines.
    #[test]
    fn the_device_breaks_the_last_tie() {
        let a = Stamp {
            wall: 7,
            counter: 0,
            device: dev(1),
        };
        let b = Stamp {
            wall: 7,
            counter: 0,
            device: dev(2),
        };
        assert_ne!(a, b);
        assert_eq!(
            a < b,
            dev(1) < dev(2),
            "decided by the id, deterministically"
        );
    }

    /// Two devices that both write, sync both ways, and must agree on the
    /// result — the property the whole no-coordinator design rests on.
    #[test]
    fn both_devices_fold_to_the_same_order() {
        let (mut m1, mut m2) = (HybridClock::new(dev(1)), HybridClock::new(dev(2)));
        let a = m1.stamp(100);
        let b = m2.stamp(120);
        m1.observe(&b, 130);
        let c = m1.stamp(130);

        let mut one = [a, b, c];
        let mut two = [c, b, a];
        one.sort();
        two.sort();
        assert_eq!(one, two, "merge order must not change the result");
        assert_eq!(one, [a, b, c]);
    }

    /// A stamp is still a time. Ordering correctly is not worth losing "what
    /// did I play on Tuesday".
    #[test]
    fn a_stamp_stays_close_to_real_time() {
        let mut clock = HybridClock::new(dev(1));
        let s = clock.stamp(1_788_115_007_000);
        assert_eq!(s.wall, 1_788_115_007_000);
    }

    #[test]
    fn a_stamp_round_trips_through_storage() {
        let mut clock = HybridClock::new(dev(9));
        let s = clock.stamp(5);
        let bytes = postcard::to_allocvec(&s).unwrap();
        assert_eq!(postcard::from_bytes::<Stamp>(&bytes).unwrap(), s);
    }
}
