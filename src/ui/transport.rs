//! **What a view may ask of playback** — #219, I11.
//!
//! ## Why a seam before a socket
//!
//! #219 ports the TUI onto `gmuse-client`. The pattern that made #213
//! uneventful was drawing the line *in process* first, so that the code being
//! moved already does not reach across it and the move becomes a rename. This
//! is that line for playback.
//!
//! Today's implementor is [`PlayerHandle`] and tomorrow's is the client. The
//! call sites do not know which, which is the property worth having before
//! fourteen thousand lines change process.
//!
//! ## It is a trait, and §12 says a trait with one implementor is a struct
//!
//! Same exception `engine::Answers` takes, for the same reason: the second
//! implementor is the point. What makes this one honest rather than speculative
//! is that the second implementor's shape is already known and already built —
//! `player.load`, `cmd.exec_line` and the `playback` topic exist and answer
//! exactly these questions.
//!
//! ## What is deliberately not here
//!
//! **Gapless next-track selection.** The decoder's `NeedNext` has a deadline and
//! the TUI answers it today from `ui.queue` and `ui.playback.scope` — state
//! §6 assigns to the control thread. A detached view cannot answer a deadline,
//! so that responsibility *moves* rather than being wrapped, and #232 is where.
//! Wrapping it here would encode a responsibility that is leaving.

use std::time::Duration;

use crate::audio::{SeekTo, State, TrackSource};
use crate::model::id::Id;
use crate::model::spine::Recording;

/// **What a view asks to be played, named both ways.**
///
/// The two transports can each use exactly one half, and neither can derive the
/// other: a local decoder needs a path and cannot mint one from a recording
/// without the projection, while a socket may only carry the recording, because
/// a locator is a property of one device's copy and I17 keeps it off the wire.
///
/// The caller has both already — `source_for` resolves the source *from* the
/// recording — so handing over both is free, and it is the alternative to a
/// transport-shaped `if` at every call site.
pub struct Loading {
    /// The user-facing grain, and the only half that crosses a socket.
    ///
    /// `None` for a file the store holds and the library has no row for: it
    /// plays locally through the `playable` fallback and cannot be named to the
    /// engine at all, which is what [`Accepted::NotNamed`] reports.
    pub recording: Option<Id<Recording>>,
    /// The composed view the play came from, in the terms the engine's `query.tree`
    /// takes — so what follows is drawn from it. `None` is the whole library.
    pub view: Option<String>,
    /// This device's copy, with its gain already resolved.
    pub source: TrackSource,
}

/// Whether a transport could act on a [`Loading`].
///
/// **One arm is unreachable per implementor, and that is the point.** A local
/// player can always play what it was handed; a remote one cannot play what the
/// library has no recording for. A view that ignored this would draw a row as
/// playing while nothing played, which is the failure the return value exists
/// to prevent.
#[must_use]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    /// Handed over. Whether it *sounds* arrives separately — over a socket that
    /// is §7.4's rule and in process it is the decoder's `Loaded`.
    Yes,
    /// The library has no recording for this file, so there is nothing to send.
    NotNamed,
}

/// **Which half of a momentary verb a key event is** — #228, §8's D-20.
///
/// `Latched` is not a third thing that happens; it is what `Press` *means* when
/// no `Release` is ever coming. It stays a distinct name because the difference
/// is worth showing a person, not because the rule branches on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyPhase {
    Press,
    Release,
}

/// What a press or release of a momentary key does, given whether one is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Momentary {
    /// Start it.
    Begin,
    /// Stop it and snap back.
    End,
    /// A release with nothing running — the tail of a latched press.
    Ignore,
}

/// **One rule for both terminals** — #228, acceptance criterion 7.
///
/// The fallback is not a second code path. On a terminal that reports release,
/// a press begins and the release ends. On one that does not, the release
/// simply never arrives, and the *next press* ends it — which is the latch,
/// falling out of the same three lines rather than being written twice.
///
/// That is what makes criterion 7 checkable rather than a promise: the same
/// function is asked both ways, and a momentary verb that became unreachable
/// without release events would have to do it here.
pub fn momentary(running: bool, phase: KeyPhase) -> Momentary {
    match (running, phase) {
        (false, KeyPhase::Press) => Momentary::Begin,
        // The latch, and the tap-through on a capable terminal: both are "it is
        // running and the key spoke again".
        (true, KeyPhase::Press) => Momentary::End,
        (true, KeyPhase::Release) => Momentary::End,
        // Nothing to end. Reached when a latched press already ended it and the
        // key is only now coming up.
        (false, KeyPhase::Release) => Momentary::Ignore,
    }
}

/// The playback a view can see and ask for.
///
/// Every method is one a socket can answer: the reads come from a `playback`
/// event's payload (§7.8) and the writes from `player.load` and
/// `cmd.exec_line`.
pub trait Transport {
    /// What the transport is doing.
    fn state(&self) -> State;

    /// How far into the current track the **sound** has got.
    ///
    /// The sounding position, not the frames handed to the device (#212's I13).
    /// Over a socket a view extrapolates this locally from `pos_frames` and
    /// `at_ns` rather than asking per frame, which is why §7.8 emits an event
    /// only on a discontinuity.
    fn position(&self) -> Duration;

    /// Start playing what is loaded.
    fn play(&self);

    /// Halt, leaving the playhead where it is.
    fn pause(&self);

    /// The cmus toggle: pause if running, resume if not.
    ///
    /// Distinct from `pause` on purpose, and #225 is the argument for why the
    /// protocol carries all three: a toggle resolved by the sender races itself,
    /// and a repeating key bound to one flutters.
    fn toggle(&self);

    /// **Return to the reference point, or set it there** — #229, D-14.
    ///
    /// This used to be `stop`, with a note saying D-10 argues stop is not a verb
    /// at all: it is pause plus a seek to the reference, which is cue. This is
    /// that change. With the reference defaulting to the top of the track, the
    /// player reading of the key — back to the start and wait — is the same
    /// command as the DJ one, and **which of the two happens is the engine's to
    /// decide** from the run state, for D-8's reason.
    fn cue(&self);

    /// Play from the reference without moving it — #228. The end of a preview
    /// is [`Transport::cue`], because a running deck already returns there.
    fn cue_preview(&self);

    /// Play this.
    ///
    /// Takes [`Loading`] rather than a path because the wire form is
    /// `player.load { recording }` and the local form is a locator, and the
    /// caller is the only place that holds both. The answer says whether this
    /// transport could act on it at all.
    fn load(&self, what: Loading) -> Accepted;

    /// **What the transport says is playing, when it is the authority.**
    ///
    /// `None` from a local player, and that is not "nothing is playing": in
    /// process the view chose the track and already knows which one. Over a
    /// socket somebody else chose — a gapless join the engine made, another
    /// view's keypress — and this is how a view finds out.
    ///
    /// Which is why the default is `None` rather than a required method: the
    /// question only has an answer where playback is somewhere else.
    fn playing(&self) -> Option<Id<Recording>> {
        None
    }

    /// **Move to the next or previous track, if the transport picks.**
    ///
    /// `false` from a local player, which is not a refusal: in process the *view*
    /// owns the queue and the pool, so it resolves the next track itself and then
    /// loads it. Over a socket the engine owns both (#232), and a view that
    /// resolved locally would pop a queue nobody plays from.
    ///
    /// So the answer means "I dealt with it", and the caller falls through to its
    /// own path when it is `false`.
    fn skip(&self, _forward: bool) -> bool {
        false
    }

    /// **Whether playback was here before this view was, and will be after.**
    ///
    /// `false` in process: the player *is* this program, so when it starts
    /// nothing is playing and the session file is the only thing that knows
    /// where it left off. `true` over a socket, where the engine may well be
    /// mid-track and its position is on the `playback` topic rather than in a
    /// file.
    ///
    /// It decides exactly one thing, and it is the property the whole milestone
    /// is for: a session restore must not `load`, `seek` and `pause` an engine
    /// that is playing. So the guard sits on the seam rather than as a flag in
    /// `main`, where the next restore path would have to remember it.
    fn outlives_this_view(&self) -> bool {
        false
    }

    /// **Whether the engine this view was driving has gone.**
    ///
    /// `false` in process, and not as a convenience: the player *is* this
    /// program, so there is no third party to lose.
    ///
    /// **Not the opposite of `outlives_this_view`** — #264, and the names are
    /// close enough to be worth separating. A view whose playback outlives it is
    /// the healthy attached case and the point of the whole milestone. This is
    /// that case having failed: still attached, nobody on the other end.
    ///
    /// It has to be asked rather than inferred, because everything a lost engine
    /// touches fails *quietly*. `holding_remote` resolves rows over the wire and
    /// `unwrap_or_default()`s a failure, so a view whose engine died draws
    /// `<missing>` for every row it had not already cached; `playing()` keeps
    /// answering with the last recording the engine named, so `gp` has no target
    /// and a rating lands on whatever was playing when the connection dropped.
    /// Each of those reads as a bug in the thing it broke. One span naming the
    /// cause is cheaper than three investigations.
    fn engine_gone(&self) -> bool {
        false
    }

    fn seek(&self, to: SeekTo);

    /// The fader position, where 1.0 is unity.
    ///
    /// **A read as well as a write, because the RT side owns the value** (§6):
    /// a view asks for a change through `param.set` and learns the result
    /// through the feedback path, rather than computing it and assuming. Reading
    /// it here is how `vol +5` knows what to add to.
    fn volume(&self) -> f32;
    fn set_volume(&self, position: f32);

    /// Times the ring ran dry while audio was owed. Diagnostics, shown in the
    /// audit line.
    fn underruns(&self) -> u32;

    /// Frames missing at the most recent underrun — how *badly* it starved.
    fn last_short_frames(&self) -> u32;

    /// **What a departed output does to playback** — #318.
    ///
    /// A no-op by default, and **not a setting accepted and ignored**: over a socket
    /// the setting reaches the engine as `set on_device_gone=…` through `cmd.exec_line`,
    /// which `apply_setting` sends for every key `not_the_engines` admits. Only a
    /// player in this process has a stream of its own to tell.
    fn set_on_device_gone(&self, _to: crate::audio::OnDeviceGone) {}
}

impl Transport for crate::audio::PlayerHandle {
    fn state(&self) -> State {
        crate::audio::PlayerHandle::state(self)
    }
    fn position(&self) -> Duration {
        crate::audio::PlayerHandle::position(self)
    }
    fn play(&self) {
        crate::audio::PlayerHandle::play(self);
    }
    fn pause(&self) {
        crate::audio::PlayerHandle::pause(self);
    }
    fn toggle(&self) {
        crate::audio::PlayerHandle::toggle(self);
    }
    fn cue(&self) {
        crate::audio::PlayerHandle::cue(self);
    }
    fn cue_preview(&self) {
        crate::audio::PlayerHandle::cue_preview(self);
    }
    /// **The recording is unused here, and deliberately not asserted on.** A
    /// local decoder opens the file; whether the library also has a row for it
    /// is a question this side does not need to ask, which is exactly why the
    /// `playable` fallback still plays a store file the library has never seen.
    fn load(&self, what: Loading) -> Accepted {
        crate::audio::PlayerHandle::load(self, what.source);
        Accepted::Yes
    }
    fn seek(&self, to: SeekTo) {
        crate::audio::PlayerHandle::seek(self, to);
    }
    fn volume(&self) -> f32 {
        crate::audio::PlayerHandle::volume(self)
    }
    fn set_volume(&self, position: f32) {
        crate::audio::PlayerHandle::set_volume(self, position);
    }
    fn underruns(&self) -> u32 {
        crate::audio::PlayerHandle::underruns(self)
    }
    fn set_on_device_gone(&self, to: crate::audio::OnDeviceGone) {
        crate::audio::PlayerHandle::set_on_device_gone(self, to);
    }
    fn last_short_frames(&self) -> u32 {
        crate::audio::PlayerHandle::last_short_frames(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// A transport that is not a `PlayerHandle`.
    ///
    /// **This is the whole point of the seam.** A trait with one implementor is
    /// a struct (§12); what makes this one earn its keep is that a second
    /// implementor is possible, and the only way to know that is to write one.
    /// When #219 lands, `gmuse-client` takes this place and the call sites do
    /// not change.
    struct Recorded {
        asked: Mutex<Vec<String>>,
        state: State,
        position: Duration,
        volume: Mutex<f32>,
    }

    impl Default for Recorded {
        fn default() -> Self {
            Self {
                asked: Mutex::default(),
                // Not `State::default()`: the enum has none, because "what is
                // the default transport state" is not a question with an answer.
                state: State::Stopped,
                position: Duration::ZERO,
                volume: Mutex::new(1.0),
            }
        }
    }

    impl Recorded {
        fn asked(&self) -> Vec<String> {
            self.asked.lock().expect("asked").clone()
        }
        fn note(&self, what: impl Into<String>) {
            self.asked.lock().expect("asked").push(what.into());
        }
    }

    impl Transport for Recorded {
        fn state(&self) -> State {
            self.state
        }
        fn position(&self) -> Duration {
            self.position
        }
        fn play(&self) {
            self.note("play");
        }
        fn pause(&self) {
            self.note("pause");
        }
        fn toggle(&self) {
            self.note("toggle");
        }
        fn cue(&self) {
            self.note("cue");
        }
        fn cue_preview(&self) {
            self.note("cue-preview");
        }
        fn load(&self, what: Loading) -> Accepted {
            self.note(format!("load {}", what.source.path.display()));
            Accepted::Yes
        }
        fn seek(&self, to: SeekTo) {
            self.note(match to {
                SeekTo::Absolute(d) => format!("seek {:.1}s", d.as_secs_f64()),
                SeekTo::Relative(d) => format!("seek {d:+.1}s"),
            });
        }
        fn volume(&self) -> f32 {
            *self.volume.lock().expect("volume")
        }
        fn set_volume(&self, position: f32) {
            *self.volume.lock().expect("volume") = position;
            self.note(format!("volume {position:.2}"));
        }
        fn underruns(&self) -> u32 {
            0
        }
        fn last_short_frames(&self) -> u32 {
            0
        }
    }

    /// Every method is reachable through the trait object, which is the shape
    /// `Context` holds. If this compiles and records, a client can take the
    /// same place.
    /// **In process there is no engine to lose** — #264.
    ///
    /// Thin on purpose, and it exists because the probe that should have found
    /// it green found it *vacuous*: flipping the trait default to `true` broke
    /// nothing in 61 integration tests, so "false in process" was a sentence in
    /// a doc comment with no instrument on it. A local player reporting the
    /// engine had gone would paint that warning across every unattached
    /// session.
    ///
    /// `Recorded` is the right subject precisely because it does not override —
    /// it takes the same default `PlayerHandle` does.
    /// **Criterion 7, as a question the code can answer** — #228.
    ///
    /// "Every momentary verb remains reachable with `hold_capable == false`" is
    /// otherwise a promise about a thing that has not happened. Here it is the
    /// same rule asked both ways: with releases, a press begins and the release
    /// ends; without them, the release never comes and the *next press* ends
    /// it. The verb is reachable either way because the fallback is not a second
    /// path — it is what the first path already does when half its events are
    /// missing.
    #[test]
    fn a_momentary_verb_is_reachable_with_and_without_key_release() {
        // A terminal that reports release: press, release.
        assert_eq!(momentary(false, KeyPhase::Press), Momentary::Begin);
        assert_eq!(momentary(true, KeyPhase::Release), Momentary::End);

        // A terminal that does not: press, press. No release ever arrives.
        assert_eq!(momentary(false, KeyPhase::Press), Momentary::Begin);
        assert_eq!(momentary(true, KeyPhase::Press), Momentary::End);
    }

    /// **A release with nothing running does nothing** — #228.
    ///
    /// Reached on a capable terminal when a latched press already ended the
    /// preview and the key is only now coming up — and by any release that
    /// arrives without its press, which is what happens the first time flags are
    /// pushed while a key is already down.
    #[test]
    fn a_release_with_nothing_running_is_ignored() {
        assert_eq!(momentary(false, KeyPhase::Release), Momentary::Ignore);
    }

    /// **The rule never leaves a preview it cannot end** — #228.
    ///
    /// Walked as a state machine over every phase, because the failure that
    /// matters is not one wrong answer: it is a sequence that arrives at
    /// "running" with nothing able to stop it, which is a deck stuck playing
    /// from the reference.
    ///
    /// The count is the instrument — an empty phase list would satisfy every
    /// assertion in the loop without running one.
    #[test]
    fn every_sequence_of_presses_and_releases_can_still_be_ended() {
        let phases = [
            KeyPhase::Press,
            KeyPhase::Release,
            KeyPhase::Press,
            KeyPhase::Press,
            KeyPhase::Release,
            KeyPhase::Release,
        ];
        assert!(phases.len() >= 6, "not enough phases to reach a bad state");

        let mut running = false;
        for phase in phases {
            running = match momentary(running, phase) {
                Momentary::Begin => true,
                Momentary::End => false,
                Momentary::Ignore => running,
            };
            // Whatever we are in, a press can always get out of it.
            let escape = momentary(running, KeyPhase::Press);
            assert_eq!(
                escape,
                if running {
                    Momentary::End
                } else {
                    Momentary::Begin
                },
                "no press could change a preview that was {running}"
            );
        }
    }

    #[test]
    fn a_local_transport_never_says_the_engine_has_gone() {
        let local = Recorded::default();
        assert!(
            !local.engine_gone(),
            "a transport that is this process reported losing an engine"
        );
    }

    #[test]
    fn a_transport_that_is_not_the_player_can_be_driven_through_the_seam() {
        let fake = Recorded {
            state: State::Playing,
            position: Duration::from_secs(12),
            volume: Mutex::new(1.0),
            ..Default::default()
        };
        let transport: &dyn Transport = &fake;

        assert_eq!(transport.state(), State::Playing);
        assert_eq!(transport.position(), Duration::from_secs(12));

        transport.play();
        transport.toggle();
        transport.seek(SeekTo::Relative(-5.0));
        transport.set_volume(0.5);
        transport.cue();

        assert_eq!(
            fake.asked(),
            vec!["play", "toggle", "seek -5.0s", "volume 0.50", "cue"],
            "the seam carried every verb through"
        );
        assert_eq!(transport.volume(), 0.5, "and a read sees the write");
    }

    /// **The reads a socket must be able to answer.** Each one comes from a
    /// `playback` event's payload (§7.8) or from `param.set`'s feedback — none
    /// of them needs a handle in this process, which is what makes the port
    /// possible rather than merely tidy.
    #[test]
    fn nothing_on_the_seam_needs_to_be_in_this_process() {
        let fake = Recorded::default();
        let transport: &dyn Transport = &fake;
        // `state` and `position` are the payload; the rest are commands.
        let _ = (transport.state(), transport.position());
        let _ = (transport.underruns(), transport.last_short_frames());
        assert!(
            fake.asked().is_empty(),
            "a read asked the transport for nothing"
        );
    }
}
