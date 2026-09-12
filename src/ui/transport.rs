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
//! the TUI answers it today from `ui.queue` and `ui.playback.aaa_mode` — state
//! §6 assigns to the control thread. A detached view cannot answer a deadline,
//! so that responsibility *moves* rather than being wrapped, and #232 is where.
//! Wrapping it here would encode a responsibility that is leaving.

use std::time::Duration;

use crate::audio::{SeekTo, State, TrackSource};

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

    /// Halt and return to the start.
    ///
    /// `transport-keymap-spec.md` D-10 argues this is not a verb at all — it is
    /// pause plus a seek to the reference point, which is cue. It is here
    /// because it is what the player does today; #229 is where it stops being
    /// its own thing.
    fn stop(&self);

    /// Play this file.
    ///
    /// **Takes a source rather than a recording, for now.** I17 says only
    /// user-facing grains cross a wire, and a path is not one — the wire form is
    /// `player.load { recording }`, and the engine resolves it through the
    /// projection. Keeping the in-process signature means this commit changes
    /// no behaviour; #219 changes the argument when the caller becomes a client.
    fn load(&self, source: TrackSource);

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
    fn stop(&self) {
        crate::audio::PlayerHandle::stop(self);
    }
    fn load(&self, source: TrackSource) {
        crate::audio::PlayerHandle::load(self, source);
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
        fn stop(&self) {
            self.note("stop");
        }
        fn load(&self, source: TrackSource) {
            self.note(format!("load {}", source.path.display()));
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
        transport.stop();

        assert_eq!(
            fake.asked(),
            vec!["play", "toggle", "seek -5.0s", "volume 0.50", "stop"],
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
