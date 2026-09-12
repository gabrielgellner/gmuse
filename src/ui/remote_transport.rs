//! **Playback over the socket** — #219, I11.
//!
//! The second implementor of [`Transport`], and the reason that trait exists.
//! With this the TUI needs no audio device of its own: it asks the engine and
//! renders what comes back, which is what I11 requires of every view.
//!
//! ## A cache, because §7.8 forbids asking
//!
//! `Transport::state` and `position` are synchronous reads, and the wire has no
//! synchronous read of them — by design. §7.8: the engine "never emits a steady
//! position stream", only an event when the state, rate or position changes
//! discontinuously, and "clients extrapolate position locally and redraw on
//! their own timer".
//!
//! So this holds the last `playback` event and extrapolates between them. A
//! reader thread keeps the cache fresh; nothing blocks a redraw on a round trip.
//!
//! ## The clock, and what is honestly approximated
//!
//! §7.9 wants `offset = engine_now_ns − client_monotonic_at_receive`, refined
//! with `time.now`, and is explicit that the two clocks share no epoch. What
//! this does instead is simpler and slightly worse: it stamps each event with a
//! local `Instant` on arrival and extrapolates from that.
//!
//! The error is one-way socket latency — microseconds on a unix socket, per
//! §7.9's own note — against a position redrawn a few times a second. What it
//! avoids is a clock estimate that drifts silently when the offset is stale. If
//! a deck ever needs beat-accurate display, §7.9's offset is the upgrade and
//! `engine_now_ns` is already in the handshake.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gmuse_client::Client;
use gmuse_proto::Topic;

use crate::audio::{SeekTo, State, TrackSource};
use crate::ui::transport::Transport;

/// The last thing the engine said about playback, and when it said it.
#[derive(Debug, Clone)]
struct Heard {
    state: State,
    /// Frames at the moment the event was published.
    pos_frames: u64,
    sample_rate: u32,
    /// When this arrived here, for extrapolating forward.
    at: Instant,
}

impl Default for Heard {
    fn default() -> Self {
        Self {
            state: State::Stopped,
            pos_frames: 0,
            sample_rate: 48_000,
            at: Instant::now(),
        }
    }
}

/// Playback, asked for over the socket.
pub struct Remote {
    client: Arc<Client>,
    heard: Arc<Mutex<Heard>>,
    /// Kept so the reader stops when this does.
    _reader: Arc<ReaderStop>,
}

struct ReaderStop(Arc<std::sync::atomic::AtomicBool>);

impl Drop for ReaderStop {
    fn drop(&mut self) {
        self.0.store(true, std::sync::atomic::Ordering::Release);
    }
}

impl Remote {
    /// Subscribe to `playback` and start keeping the cache fresh.
    pub fn attach(client: Arc<Client>) -> Result<Self, gmuse_client::ClientError> {
        client.subscribe(&[Topic::Playback])?;

        let heard = Arc::new(Mutex::new(Heard {
            sample_rate: client.hello().sample_rate.max(1),
            ..Heard::default()
        }));
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));

        {
            let heard = Arc::clone(&heard);
            let client = Arc::clone(&client);
            let stopping = Arc::clone(&stop);
            std::thread::Builder::new()
                .name("gmuse-tui-playback".into())
                .spawn(move || {
                    while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                        let Ok(event) = client.next_event(Duration::from_millis(250)) else {
                            continue;
                        };
                        if event.topic != Topic::Playback {
                            continue;
                        }
                        let mut held = heard.lock().expect("heard");
                        if let Some(state) = event.body["state"].as_str() {
                            held.state = match state {
                                "playing" => State::Playing,
                                "paused" => State::Paused,
                                _ => State::Stopped,
                            };
                        }
                        // **Absent is not zero.** A `playback` event before any
                        // stream has published a mapping carries `null`, and
                        // taking that as frame 0 would jerk a playhead back to
                        // the start of the track.
                        if let Some(frames) = event.body["pos_frames"].as_u64() {
                            held.pos_frames = frames;
                        }
                        if let Some(rate) = event.body["sample_rate"].as_u64() {
                            held.sample_rate = (rate as u32).max(1);
                        }
                        held.at = Instant::now();
                    }
                })
                .ok();
        }

        Ok(Self {
            client,
            heard,
            _reader: Arc::new(ReaderStop(stop)),
        })
    }

    /// Send a command through the one textual command language (§7.12).
    ///
    /// **Fire and forget.** §7.4's acknowledgement semantics say a result means
    /// *accepted*, not *executed* — whether it sounded arrives as a `playback`
    /// event. So a view that waited here would be waiting for the wrong thing,
    /// and waiting at all would put a socket round trip inside a keypress.
    fn line(&self, line: &str) {
        let _ = self.client.notify(
            gmuse_proto::request::method::EXEC_LINE,
            Some(serde_json::json!({ "line": line })),
        );
    }
}

impl Transport for Remote {
    fn state(&self) -> State {
        self.heard.lock().expect("heard").state
    }

    fn position(&self) -> Duration {
        let held = self.heard.lock().expect("heard");
        let played = Duration::from_secs_f64(held.pos_frames as f64 / f64::from(held.sample_rate));
        match held.state {
            // Extrapolated, per §7.8. The engine said where it was and when;
            // between events the clock is what moves.
            State::Playing => played + held.at.elapsed(),
            State::Paused | State::Stopped => played,
        }
    }

    fn play(&self) {
        self.line("play");
    }

    fn pause(&self) {
        self.line("pause");
    }

    fn toggle(&self) {
        self.line("pause");
    }

    fn stop(&self) {
        self.line("stop");
    }

    /// **A path cannot cross** — I17: only user-facing grains go on the wire, and
    /// a locator is a property of one device's copy.
    ///
    /// Nothing calls this on a remote transport: a view loads by asking the
    /// engine for a recording, which is `player.load`. It is unreachable rather
    /// than unimplemented, and saying so is better than sending a path the
    /// engine would have to refuse.
    fn load(&self, _source: TrackSource) {
        debug_assert!(
            false,
            "a remote transport loads by recording, not by path — use player.load"
        );
    }

    fn seek(&self, to: SeekTo) {
        match to {
            SeekTo::Absolute(d) => self.line(&format!("seek {}", d.as_secs())),
            SeekTo::Relative(d) => self.line(&format!("seek {d:+}")),
        }
    }

    fn volume(&self) -> f32 {
        // Not on the `playback` payload: §7.8 puts parameter values on `params`,
        // which has no producer yet (#230 scoped it out). Unity until it does,
        // which is wrong by whatever the fader is set to and is not wrong about
        // anything a view acts on.
        1.0
    }

    fn set_volume(&self, position: f32) {
        self.line(&format!("vol {}", (position * 100.0).round() as i32));
    }

    fn underruns(&self) -> u32 {
        0
    }

    fn last_short_frames(&self) -> u32 {
        0
    }
}
