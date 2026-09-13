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

use crate::audio::{SeekTo, State};
use crate::model::id::Id;
use crate::model::spine::Recording;
use crate::ui::transport::{Accepted, Loading, Transport};

/// The last thing the engine said about playback, and when it said it.
#[derive(Debug, Clone)]
struct Heard {
    state: State,
    /// Frames at the moment the event was published.
    pos_frames: u64,
    sample_rate: u32,
    /// **Who chose it does not matter, only that the engine says so.** This is
    /// how an attached view learns about a gapless join it did not ask for, and
    /// about another view's keypress.
    recording: Option<Id<Recording>>,
    /// **How fast the position is moving, as a multiple of real time** — #212, I13.
    ///
    /// One while a stream is running and **zero when the device has gone**: the render
    /// step has stopped, so the engine's position is frozen. Without this a remote view
    /// took that frozen position, reset its own clock and advanced forever while nothing
    /// played — which is the failure I13 is about, on the one hardware event #212 calls
    /// the likeliest in this design.
    ///
    /// It is also where varispeed arrives when there is any, which is why the field was
    /// already on the payload as a constant.
    rate: f64,
    /// When this arrived here, for extrapolating forward.
    at: Instant,
}

impl Default for Heard {
    fn default() -> Self {
        Self {
            state: State::Stopped,
            pos_frames: 0,
            sample_rate: 48_000,
            recording: None,
            rate: 1.0,
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

/// Take a `playback` body into the cache.
///
/// **One function for the event and for the snapshot** — #248. They carry the same
/// fields by design, and two readers of one shape is how a seeded view comes to
/// disagree with the stream that then updates it. `at_ns` is the only field the
/// snapshot omits, and this never used it: §7.8 lets a client extrapolate from its
/// own clock, so arrival is the instant that matters here.
fn absorb(held: &mut Heard, body: &serde_json::Value) {
    if let Some(state) = body["state"].as_str() {
        held.state = match state {
            "playing" => State::Playing,
            "paused" => State::Paused,
            _ => State::Stopped,
        };
    }
    // **Absent is not zero.** A `playback` event before any stream has published a
    // mapping carries `null`, and taking that as frame 0 would jerk a playhead back
    // to the start of the track.
    if let Some(frames) = body["pos_frames"].as_u64() {
        held.pos_frames = frames;
    }
    if let Some(rate) = body["sample_rate"].as_u64() {
        held.sample_rate = (rate as u32).max(1);
    }
    // **Absent means one, not zero.** An engine that predates the field is running a
    // stream; defaulting to zero would freeze a playhead that is moving.
    held.rate = body["rate"].as_f64().unwrap_or(1.0);
    // **Set on every event, `null` included.** Absent is not "unchanged" here the way
    // `pos_frames` is: a stop publishes no recording, and keeping the last one would
    // leave a view drawing a track as current after playback had ended.
    held.recording = body["recording"]
        .as_str()
        .and_then(|s| s.parse::<uuid::Uuid>().ok())
        .map(Id::from_uuid);
    held.at = Instant::now();
}

impl Remote {
    /// Subscribe to `playback`, ask what is playing, and keep the cache fresh.
    pub fn attach(client: Arc<Client>) -> Result<Self, gmuse_client::ClientError> {
        client.subscribe(&[Topic::Playback])?;

        let mut seed = Heard {
            sample_rate: client.hello().sample_rate.max(1),
            ..Heard::default()
        };
        // **Asked, because the stream will not say** — #248, #243.
        //
        // The `playback` topic carries discontinuities and §7.8 forbids a steady
        // position stream, so a view that attached between two of them believed
        // nothing was playing until the next track change — which mid-album is the
        // length of a track. `gp` had no target and the status line said stopped over
        // audible music.
        //
        // **After `subscribe`, and the order is load-bearing.** An event that lands
        // between the two is newer than this snapshot and overwrites it on arrival.
        // Seeding first and subscribing second is the version that drops an event.
        //
        // A refusal is not fatal: an engine too old to answer is one this view can
        // still drive, and it degrades to the behaviour that prompted the issue
        // rather than to no view at all.
        if let Ok(now) = client.call(gmuse_proto::method::PLAYBACK_GET, None) {
            absorb(&mut seed, &now);
        }

        let heard = Arc::new(Mutex::new(seed));
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
                        absorb(&mut held, &event.body);
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

    /// The connection, for the calls that are not transport.
    ///
    /// `library.scan` is the one that needs it today: a rescan is a write, so
    /// I15 sends it over the socket rather than letting an attached view fold
    /// and append beside the engine.
    pub fn client(&self) -> &Arc<Client> {
        &self.client
    }

    /// What the engine's device is running at, for the audit line.
    ///
    /// From the handshake until the first `playback` event says otherwise — a
    /// rate change is a discontinuity §7.8 publishes, so this follows it.
    pub fn device_rate(&self) -> u32 {
        self.heard.lock().expect("heard").sample_rate
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

impl Remote {
    /// **Where the sound is now, from what the engine last said** — #212, §7.8.
    ///
    /// Its own function so the arithmetic can be tested without a socket: it is the
    /// part that was wrong, and a connection adds nothing to the question.
    fn extrapolated(held: &Heard) -> Duration {
        let played = Duration::from_secs_f64(held.pos_frames as f64 / f64::from(held.sample_rate));
        match held.state {
            // The engine said where it was and when; between events the clock is what
            // moves — **times the rate**, which is zero when the device has gone.
            State::Playing => played + held.at.elapsed().mul_f64(held.rate),
            State::Paused | State::Stopped => played,
        }
    }
}

impl Transport for Remote {
    fn state(&self) -> State {
        self.heard.lock().expect("heard").state
    }

    fn position(&self) -> Duration {
        Self::extrapolated(&self.heard.lock().expect("heard"))
    }

    /// **Resume, which is `pause` when paused** — #225.
    ///
    /// `play` is not on the wire and that is deliberate: it starts the *selected*
    /// track, so it needs a cursor. Resuming is `pause`, which toggles — so this
    /// has to know the run state, and it does, from the event stream.
    ///
    /// #225 is the argument for the protocol carrying three distinct verbs, and
    /// this is exactly the case it names: a toggle resolved by the sender races
    /// itself. The race is one keypress wide and `play` is not a repeating key, so
    /// it is a small wrong rather than the fluttering one #225 is really about.
    fn play(&self) {
        if self.state() != State::Playing {
            self.line("pause");
        }
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

    /// **The recording crosses and the path does not** — I17.
    ///
    /// `what.source` is ignored on purpose: a locator is a property of one
    /// device's copy, and the engine resolves the recording through its own
    /// projection, which is the copy *it* can open. On the same machine that is
    /// the same file; the point is that nothing here assumes it.
    ///
    /// A store file the library has no row for cannot be named, so it cannot be
    /// played by an attached view. That is reported rather than swallowed.
    fn load(&self, what: Loading) -> Accepted {
        let Some(recording) = what.recording else {
            return Accepted::NotNamed;
        };
        let _ = self.client.notify(
            gmuse_proto::request::method::PLAYER_LOAD,
            Some(serde_json::json!({ "recording": recording.to_string() })),
        );
        Accepted::Yes
    }

    /// **The engine picks, because it owns the queue and the pool** — #232, #233.
    ///
    /// `prev` works too, and used to be refused: `next::prev` walks the pool
    /// backwards and never reads the queue, so "the previous row in the pool's
    /// order" is what it always meant in a view and is now what it means here.
    fn skip(&self, forward: bool) -> bool {
        self.line(if forward { "next" } else { "prev" });
        true
    }

    fn playing(&self) -> Option<Id<Recording>> {
        self.heard.lock().expect("heard").recording
    }

    /// The engine was here first and stays after. That is what attaching means.
    fn outlives_this_view(&self) -> bool {
        true
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

#[cfg(test)]
mod tests {
    use super::*;

    /// **A `playback.get` snapshot seeds the cache the same way an event does** —
    /// #248, #243.
    ///
    /// The two bodies carry the same fields by design, and `absorb` is deliberately one
    /// function so a seeded view cannot disagree with the stream that then updates it.
    /// The one difference is that a snapshot has no `at_ns` — so this uses a body with
    /// the field genuinely absent, which is the shape that would have caught a reader
    /// that depended on it.
    #[test]
    fn a_snapshot_tells_a_fresh_view_what_is_playing() {
        let recording = uuid::Uuid::new_v4();
        // Exactly what `playback.get` answers: no `at_ns`, and nothing else added.
        let snapshot = serde_json::json!({
            "state": "playing",
            "recording": recording.to_string(),
            "pos_frames": 96_000,
            "stream_generation": 2,
            "sample_rate": 44_100,
            "rate": 1.0,
        });

        let mut held = Heard::default();
        // **The instrument is attached**: a default cache is the state the bug left a
        // view in, so asserting it beforehand is what makes the change below mean
        // something rather than describing a cache that was already right.
        assert_eq!(
            held.state,
            State::Stopped,
            "the default is not the bug's state"
        );
        assert!(held.recording.is_none());

        absorb(&mut held, &snapshot);

        assert_eq!(held.state, State::Playing);
        assert_eq!(
            held.recording.map(|id| id.as_uuid()),
            Some(recording),
            "a view that asked was still not told which track"
        );
        assert_eq!(held.sample_rate, 44_100, "the engine's rate was ignored");
        // Two seconds at 48k is not two seconds at 44.1k, which is why the rate has to
        // come from the same body rather than from the handshake.
        assert_eq!(
            Remote::extrapolated(&Heard {
                state: State::Paused,
                ..held
            }),
            Duration::from_secs_f64(96_000.0 / 44_100.0)
        );
    }

    /// **A playhead does not advance while nothing is sounding** — #212, I13.
    ///
    /// §7.8 gives a client no synchronous read of position, so it extrapolates between
    /// discontinuities. When the device goes the render step stops and the engine's
    /// position freezes — and a view that kept extrapolating showed a playhead moving
    /// through audio nobody could hear, for ever. #212 calls a device disappearing the
    /// likeliest hardware event in this design.
    ///
    /// `rate` is what carries it. Tested on the cache directly, because the arithmetic
    /// is the thing that was wrong and a socket adds nothing to the question.
    #[test]
    fn a_rate_of_zero_stops_the_position_advancing() {
        let held = |rate: f64| Heard {
            state: State::Playing,
            pos_frames: 48_000,
            sample_rate: 48_000,
            recording: None,
            rate,
            // A second ago, so any extrapolation is visible.
            at: Instant::now() - Duration::from_secs(1),
        };

        let running = Remote::extrapolated(&held(1.0));
        assert!(
            running >= Duration::from_millis(1_900),
            "a running stream advances: {running:?}"
        );

        let stalled = Remote::extrapolated(&held(0.0));
        assert_eq!(
            stalled,
            Duration::from_secs(1),
            "a lost device leaves the playhead where the sound stopped"
        );
    }

    /// **An engine that predates the field is running a stream, not a stalled one.**
    ///
    /// Defaulting an absent `rate` to zero would freeze a playhead that is moving,
    /// which is a worse failure than the one this fixes.
    #[test]
    fn an_absent_rate_reads_as_one() {
        assert_eq!(Heard::default().rate, 1.0);
    }
}
