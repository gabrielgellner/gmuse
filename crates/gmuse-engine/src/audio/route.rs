//! **Which output the stream is on, and whether a reroute left one behind** — #318.
//!
//! cpal reports a reroute of a default-device stream as `DeviceChanged` and nothing
//! else: not where it went, not where it came from, not why. Two quite different
//! things arrive as that one event:
//!
//! - **A device arrived**, or the default was switched by hand. Headphones went in,
//!   or somebody picked the speakers in Control Center. Following it is right, and
//!   is what the 2026-08-27 Decision Log entry built.
//! - **The device we were on left.** Bluetooth headphones went to sleep, and the OS
//!   moved the music to the laptop speakers. Following it is how a library gets
//!   played to a room nobody asked to play it to.
//!
//! **Telling them apart is a question about the old device, not the new one**, which
//! is Apple's `oldDeviceUnavailable` and Android's `AUDIO_BECOMING_NOISY`: is the
//! output we were on still present? That is what [`rerouted`] asks, as a pure
//! function so it can be tested without a sound card.
//!
//! **Only off the real-time thread.** Enumerating devices is a pile of syscalls; this
//! runs on the decoder thread, which already turns the callback's flags into events.

use std::time::{Duration, Instant};

/// **What a reroute did**, judged by whether the output we were on is still present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reroute {
    /// The output we were on is still there: something arrived, or the default was
    /// switched by hand. Follow it, as before #318.
    Followed,
    /// The output we were on is gone.
    Left,
}

/// **The rule, as a function of two lists** — #318.
///
/// `present` is every output the host lists now. `None` is a list that could not be
/// read, and **it cannot vouch for anything**: the cost of a wrong answer is not
/// symmetric. Pausing when the device was still there costs one keypress; playing on
/// when it was not is the bug.
pub fn rerouted<T: PartialEq>(was_on: &T, present: Option<&[T]>) -> Reroute {
    match present {
        Some(present) if present.contains(was_on) => Reroute::Followed,
        _ => Reroute::Left,
    }
}

/// **How long after a reroute to look again.**
///
/// The default-output listener and the device list are separate CoreAudio
/// properties, and nothing promises the list has caught up when the default has
/// moved. A device that is gone but still listed would read as [`Reroute::Followed`],
/// so a `Followed` is asked once more after this. Half a second is far enough that a
/// list which was going to catch up has, and short enough that a missed departure
/// costs half a second of the speakers rather than a song.
pub const RECHECK: Duration = Duration::from_millis(500);

/// An output, as the decoder needs to know it: by id to compare, by name to say.
#[derive(Debug, Clone)]
pub struct Output {
    pub id: cpal::DeviceId,
    pub name: String,
}

/// **Which output the stream is on, as far as the decoder knows** — #318.
///
/// The output it was *opened* on is only the first answer. After a reroute the stream
/// is on the new default, and it is *that* device leaving which matters next: put the
/// headphones on mid-song and take them off again, and the second reroute is about
/// the headphones, not the speakers it started on.
#[derive(Debug)]
pub struct Route {
    /// `None` for a headless player, or a device that would not say who it was.
    /// Either way there is nothing to have left.
    on: Option<Output>,
    /// A `Followed` to look at again, and when — see [`RECHECK`].
    recheck: Option<(Instant, Output)>,
}

/// What the decoder should do about a reroute or a recheck.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing left: say the output changed, as before.
    Followed,
    /// This output is gone.
    Left(String),
}

impl Route {
    /// On this output.
    pub fn on(output: Option<Output>) -> Self {
        Self {
            on: output,
            recheck: None,
        }
    }

    /// **The OS rerouted the stream.** Judge the output we were on, then move to
    /// wherever the stream went.
    ///
    /// `now` is the default output after the reroute, which is where cpal's default
    /// stream has gone; `present` is the host's output list, `None` if it could not be
    /// read. Both are asked by the caller so this stays a function of what it is told.
    pub fn rerouted(
        &mut self,
        at: Instant,
        now: Option<Output>,
        present: Option<&[cpal::DeviceId]>,
    ) -> Verdict {
        let was = std::mem::replace(&mut self.on, now);
        // A recheck of an earlier reroute is superseded: this one judges the output
        // we were on more recently.
        self.recheck = None;
        let Some(was) = was else {
            return Verdict::Followed;
        };
        match rerouted(&was.id, present) {
            Reroute::Left => Verdict::Left(was.name),
            Reroute::Followed => {
                self.recheck = Some((at + RECHECK, was));
                Verdict::Followed
            }
        }
    }

    /// **Whether a `Followed` is owed a second look** — see [`RECHECK`].
    ///
    /// Almost always `false`, and the caller lists devices only when it is `true`:
    /// enumeration is not free, and this is asked on every pass of the decoder loop.
    pub fn due(&self, at: Instant) -> bool {
        self.recheck.as_ref().is_some_and(|(when, _)| at >= *when)
    }

    /// The recheck [`due`](Route::due) said was owed, judged against `present`.
    /// `Some(Left)` is a departure the first look missed.
    pub fn recheck(&mut self, present: Option<&[cpal::DeviceId]>) -> Option<Verdict> {
        let (_, was) = self.recheck.take()?;
        match rerouted(&was.id, present) {
            Reroute::Left => Some(Verdict::Left(was.name)),
            Reroute::Followed => None,
        }
    }

    /// **Forget a pending recheck.** For a decoder waking from idle: a recheck is
    /// about the song that was playing at the reroute, and pausing a song loaded an
    /// hour later because of it would be acting on news that went stale.
    pub fn settle(&mut self) {
        self.recheck = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On whichever host this platform has, so the tests run off macOS too.
    fn id(s: &str) -> cpal::DeviceId {
        cpal::DeviceId::new(cpal::default_host().id(), s)
    }

    fn output(s: &str) -> Output {
        Output {
            id: id(s),
            name: s.to_string(),
        }
    }

    const HEADPHONES: &str = "headphones";
    const SPEAKERS: &str = "speakers";

    /// **The fixture has two different devices in it**, or every test below compares
    /// a thing with itself and passes for that reason.
    #[test]
    fn the_fixture_names_two_devices() {
        assert_ne!(id(HEADPHONES), id(SPEAKERS));
        assert_eq!(id(HEADPHONES), id(HEADPHONES));
    }

    /// **The rule both ways** — #318's "seen to fail both ways". One list holds the
    /// device and one does not, and nothing else differs.
    #[test]
    fn a_device_still_listed_was_followed_and_one_missing_was_left() {
        let both = [id(SPEAKERS), id(HEADPHONES)];
        let speakers_only = [id(SPEAKERS)];
        assert_eq!(
            rerouted(&id(HEADPHONES), Some(&both[..])),
            Reroute::Followed
        );
        assert_eq!(
            rerouted(&id(HEADPHONES), Some(&speakers_only[..])),
            Reroute::Left
        );
    }

    /// **An unreadable list cannot vouch for the device** — the asymmetry in
    /// [`rerouted`]'s comment.
    #[test]
    fn a_list_that_could_not_be_read_vouches_for_nothing() {
        assert_eq!(rerouted(&id(HEADPHONES), None), Reroute::Left);
    }

    /// **Headphones off: the speakers are where we went, the headphones what we left.**
    #[test]
    fn taking_the_headphones_off_names_the_headphones() {
        let mut route = Route::on(Some(output(HEADPHONES)));
        let t = Instant::now();
        let verdict = route.rerouted(t, Some(output(SPEAKERS)), Some(&[id(SPEAKERS)]));
        assert_eq!(verdict, Verdict::Left("headphones".into()));
    }

    /// **Headphones on: nothing left, so nothing to pause.**
    #[test]
    fn putting_the_headphones_on_is_followed() {
        let mut route = Route::on(Some(output(SPEAKERS)));
        let t = Instant::now();
        let verdict = route.rerouted(
            t,
            Some(output(HEADPHONES)),
            Some(&[id(SPEAKERS), id(HEADPHONES)]),
        );
        assert_eq!(verdict, Verdict::Followed);
    }

    /// **The output that matters next is the one we moved to.** On, then off: the
    /// second reroute is judged on the headphones. A `Route` that kept judging the
    /// output it was opened on would answer `Followed` here, because the speakers are
    /// still present.
    #[test]
    fn the_second_reroute_is_judged_on_where_the_first_one_went() {
        let mut route = Route::on(Some(output(SPEAKERS)));
        let t = Instant::now();
        route.rerouted(
            t,
            Some(output(HEADPHONES)),
            Some(&[id(SPEAKERS), id(HEADPHONES)]),
        );
        let verdict = route.rerouted(t, Some(output(SPEAKERS)), Some(&[id(SPEAKERS)]));
        assert_eq!(verdict, Verdict::Left("headphones".into()));
    }

    /// **A device still listed at the first look is looked at again**, and a list
    /// that has since caught up says it left.
    #[test]
    fn a_departure_the_first_look_missed_is_caught_by_the_second() {
        let mut route = Route::on(Some(output(HEADPHONES)));
        let t = Instant::now();
        // The stale list: the headphones are still in it.
        let first = route.rerouted(
            t,
            Some(output(SPEAKERS)),
            Some(&[id(SPEAKERS), id(HEADPHONES)]),
        );
        assert_eq!(first, Verdict::Followed);

        assert!(!route.due(t), "a recheck is not due the instant it is set");
        assert!(route.due(t + RECHECK), "and is due after RECHECK");
        assert_eq!(
            route.recheck(Some(&[id(SPEAKERS)])),
            Some(Verdict::Left("headphones".into()))
        );
        assert!(!route.due(t + RECHECK * 10), "a recheck is answered once");
    }

    /// **A device still there at the recheck was followed**, and says nothing.
    #[test]
    fn a_device_still_there_at_the_recheck_was_followed() {
        let mut route = Route::on(Some(output(HEADPHONES)));
        let t = Instant::now();
        route.rerouted(
            t,
            Some(output(SPEAKERS)),
            Some(&[id(SPEAKERS), id(HEADPHONES)]),
        );
        assert!(route.due(t + RECHECK));
        assert_eq!(route.recheck(Some(&[id(SPEAKERS), id(HEADPHONES)])), None);
    }

    /// **A settled route owes nothing**, so a decoder waking from idle does not
    /// pause the next song on an old reroute's account.
    #[test]
    fn settling_forgets_the_recheck() {
        let mut route = Route::on(Some(output(HEADPHONES)));
        let t = Instant::now();
        route.rerouted(
            t,
            Some(output(SPEAKERS)),
            Some(&[id(SPEAKERS), id(HEADPHONES)]),
        );
        assert!(route.due(t + RECHECK), "the instrument: a recheck was owed");
        route.settle();
        assert!(!route.due(t + RECHECK));
    }

    /// **A player that never knew its output has nothing to have left** — a headless
    /// one, or a device that would not give its id.
    #[test]
    fn no_known_output_is_never_left() {
        let mut route = Route::on(None);
        let verdict = route.rerouted(Instant::now(), Some(output(SPEAKERS)), Some(&[]));
        assert_eq!(verdict, Verdict::Followed);
    }
}
