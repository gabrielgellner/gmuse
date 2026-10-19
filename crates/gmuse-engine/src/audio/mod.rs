//! The audio subsystem: `PlayerHandle` is the whole public API the UI talks to.
//!
//! Threading follows DESIGN §3 (INVARIANT):
//!
//! ```text
//!   caller (UI)            decoder thread            audio callback (cpal, RT)
//!   ───────────            ──────────────            ─────────────────────────
//!   PlayerHandle    cmd    demux, decode, resample   drain rtrb consumer
//!   methods        ───▶    push frames to rtrb  ───▶ apply gain, write output
//!   reads atomics  ◀───                              update atomics only
//!                   evt
//! ```

mod decode;
pub mod device;
pub mod null;
pub mod resample;
mod ring;
pub mod route;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, TrySendError, bounded};

/// Everything the player needs from the command line, parsed once.
#[derive(Debug, Clone, Default)]
pub struct PlayerConfig {
    /// Substring match against the output device id. `None` picks the default.
    pub device: Option<String>,
    /// Force the device rate. `None` uses whatever the device prefers.
    pub rate: Option<u32>,
    pub resample: resample::Quality,
    pub volume: f32,
    /// What to do when the output we were playing on leaves — #318.
    pub on_device_gone: OnDeviceGone,
}

/// **What to do when the output we were playing on leaves** — #318.
///
/// Headphones going to sleep make the OS move the stream to the speakers. Apple
/// pauses there (`oldDeviceUnavailable`) and Android tells a player to
/// (`AUDIO_BECOMING_NOISY`), and that is the default; `Continue` is the behaviour
/// before #318, for anyone who wants the music to follow the OS wherever it goes.
///
/// **Only the leaving direction.** A device arriving, or a default switched by hand,
/// is followed either way: plugging headphones in mid-song is not a reason to stop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDeviceGone {
    #[default]
    Pause,
    Continue,
}

impl OnDeviceGone {
    pub const ALL: [OnDeviceGone; 2] = [OnDeviceGone::Pause, OnDeviceGone::Continue];

    pub fn label(self) -> &'static str {
        match self {
            OnDeviceGone::Pause => "pause",
            OnDeviceGone::Continue => "continue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.label() == s)
    }
}

/// **Which transport verb** — #225, `transport-keymap-spec.md` D-6 to D-8.
///
/// The audio layer's own spelling. `Instruction` is a layer above this one and
/// `gmuse_proto::request::Transport` is the wire's; all three say the same three words,
/// because the distinction has to survive every hop or the sender ends up resolving it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Play,
    Pause,
    Toggle,
}

/// **What one press of cue does**, given where the transport is — #229, D-14.
///
/// Cue is two meanings in one key and **the state chooses between them**, which is
/// why this is resolved here rather than by whoever pressed it: a sender deciding
/// "am I playing? then return" reads a snapshot that may already be wrong, and that
/// is D-8's race with a different verb's name on it.
///
/// D-14 is what makes the player and the DJ readings the same verb: **the reference
/// point starts at the top of the track**, so on a player that has never marked
/// anything, cue is "back to the start and wait" — cmus's stop, minus the stopping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cueing {
    /// Paused, so the playhead is where the reference should be.
    MarkHere,
    /// Running, so go back to the reference and wait there.
    ReturnAndPause,
    /// Nothing loaded: no playhead to mark, nowhere to return to.
    Nothing,
}

/// The rule, as a function of state alone. See [`Cueing`].
pub fn cued(state: State, loaded: bool) -> Cueing {
    match (loaded, state) {
        (false, _) => Cueing::Nothing,
        (true, State::Playing) => Cueing::ReturnAndPause,
        // Stopped counts as not running. Its playhead is the top of the track,
        // so marking there is the default it already had — harmless, and one
        // case fewer than treating it as its own thing.
        (true, State::Paused | State::Stopped) => Cueing::MarkHere,
    }
}

/// **What a transport verb does to the run state** — #225, D-7 and D-8.
///
/// A pure function of the three things that decide it, so idempotence is testable without a
/// decoder thread, a device or a sound card — the shape `which_key_due` takes, for the same
/// reason.
///
/// **One store, never a read-modify-write.** The caller stores what this returns, and the
/// render callback reads `State::from_u8` every buffer: a transition applied as read-then-write
/// is one the callback can observe mid-flight, and D-8's whole argument is about who is allowed
/// to resolve a state question.
///
/// `loaded` is whether a track is open. Nothing starts without one, which is what makes `Play`
/// safe to send at anything.
pub fn transported(state: State, verb: Verb, loaded: bool) -> State {
    match verb {
        // **Idempotent**: already playing stays playing, and a held key without the kitty
        // protocol arrives as repeats.
        Verb::Play if loaded => State::Playing,
        Verb::Play => state,
        // **Idempotent**, and the verb a broadcast needs: pausing what is already paused must
        // leave it paused, where toggling it would start it.
        Verb::Pause if state == State::Playing => State::Paused,
        Verb::Pause => state,
        Verb::Toggle => match state {
            State::Playing => State::Paused,
            _ if loaded => State::Playing,
            _ => State::Stopped,
        },
    }
}

/// What the player is doing. Stored as a `u8` so the RT callback can read it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum State {
    Stopped = 0,
    Playing = 1,
    Paused = 2,
}

impl State {
    fn from_u8(v: u8) -> Self {
        match v {
            1 => State::Playing,
            2 => State::Paused,
            _ => State::Stopped,
        }
    }
}

/// Where to seek to.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SeekTo {
    /// An absolute position from the start of the track.
    Absolute(Duration),
    /// Forwards (positive) or backwards (negative) from the current position.
    Relative(f64),
}

/// A track to play, and the gain to play it at.
///
/// The gain is applied by the *decoder*, not the callback, so it travels
/// through the ring with the audio it belongs to. That is what makes it land
/// exactly at a gapless join, where the decoder is a ring's depth ahead of
/// what the listener is hearing — a per-track gain applied at the output
/// would otherwise hit the last second of the previous track.
///
/// It is just a number here. The audio layer does not know it came from a
/// ReplayGain tag.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackSource {
    pub path: PathBuf,
    pub gain: f32,
}

impl TrackSource {
    /// Play a file untouched, at unity gain.
    ///
    /// **Test-only, and that is the point.** Every source in the running
    /// program is built from a `Track` by `source_for`, so nothing plays that
    /// the library does not know — the shadow "playing something untracked"
    /// state that `--repl` and a bare file argument created is gone with them.
    ///
    /// The decoder's own tests still want it, and may have it: this layer sits
    /// *below* the store and is deliberately ignorant of `CopyId`. A path and
    /// a gain is the whole of what it needs, which is why testing it does not
    /// require a library.
    #[cfg(test)]
    pub fn plain(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            gain: 1.0,
        }
    }
}

/// Commands sent to the decoder thread.
#[derive(Debug, Clone)]
pub enum PlayerCmd {
    Load(TrackSource),
    Play,
    Pause,
    Toggle,
    Stop,
    /// Return to the reference point, or set it — [`cued`] decides which.
    Cue,
    /// **Play from the reference without moving it** — #228, §5's third case.
    ///
    /// The start of a preview. There is no matching `end`, and that is not an
    /// omission: during a preview the deck is *running*, so [`cued`] already
    /// answers `ReturnAndPause` — which is the snap back, exactly. One command
    /// rather than a pair that could disagree about where the reference is.
    CuePreview,
    Seek(SeekTo),
    /// The answer to [`PlayerEvent::NeedNext`]: what follows the current
    /// track, or `None` if nothing does.
    ///
    /// Sent *in reply*, never speculatively, so the resolution is made at the
    /// moment it is needed and cannot go stale — see `decode::run`.
    SetNext(Option<TrackSource>),
    Shutdown,
}

/// Things the decoder thread reports back.
///
/// Position is deliberately *not* an event: it comes from an atomic the RT
/// callback updates, so the UI can read it at whatever rate it renders rather
/// than being driven by a message per frame.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    /// A track became audible.
    ///
    /// For an ordinary load this is immediate. For a gapless transition it is
    /// deliberately held back until the playhead crosses the join, so it
    /// always means "this is what you are hearing now" rather than "this is
    /// what the decoder is working on" — the two are a ring's depth apart.
    Loaded {
        path: PathBuf,
        duration: Option<Duration>,
        source_rate: u32,
        channels: usize,
        /// True when the file's rate differs from the device's and is being converted.
        resampling: bool,
        /// The gain actually being applied to this track's samples. Reported
        /// by the decoder rather than remembered by the caller, so the log
        /// records what happened rather than what was asked for.
        gain: f32,
    },
    /// The current track has been decoded to its end and the ring still holds
    /// the last of it. Reply with [`PlayerCmd::SetNext`] to have the next
    /// track spliced on without a gap; reply `None` to let playback stop.
    ///
    /// The reply is needed within [`decode::NEXT_REPLY_TIMEOUT`]; after that
    /// the decoder gives up and finishes the track, which is simply the
    /// non-gapless behaviour rather than a failure.
    NeedNext,
    /// The loaded track played to its end and nothing followed it.
    Finished,
    /// Decoding or demuxing failed; the track was abandoned.
    Error(String),
    /// The output device went away (unplugged, or the stream errored).
    DeviceLost(String),
    /// The OS moved playback to a different output and the stream survived.
    /// Worth telling the user; not worth treating as a failure.
    DeviceChanged,
    /// **The output we were playing on left**, and the OS moved the stream
    /// elsewhere — #318.
    ///
    /// `paused` says whether this pause was ours: `false` under
    /// [`OnDeviceGone::Continue`], or when nothing was playing to pause.
    DeviceGone { left: String, paused: bool },
}

/// Decibels spanned by the fader, from silence to unity.
///
/// The fader position maps *linearly onto decibels*, which is the only mapping
/// that makes every step of `+`/`-` change the level by the same amount. A raw
/// amplitude multiplier does not: at a ±5% step it moves 0.45dB at the top of
/// the range and 6dB near the bottom, a thirteenfold spread, so the first
/// presses are below the ~1dB a listener can notice and the last are a cliff.
///
/// A power law (`gain = position^n`) does not fix this either — the exponent
/// scales every step by the same factor, leaving the unevenness untouched.
/// This is why mpv's cubic mapping has the same 13x spread as a linear one.
///
/// 60dB matches librespot's default and lands a 5% step on exactly 3dB. cmus
/// takes the same approach through a table copied from alsa-lib, over ~48dB.
const VOLUME_DB_RANGE: f32 = 60.0;

/// The largest gain the fader can reach: +6dB, or twice the amplitude.
///
/// Unchanged from when the fader *was* the gain — this alters the shape of the
/// control, not how loud it can get.
const MAX_GAIN: f32 = 2.0;

/// Fader position at unity gain. Above this the fader boosts, to `MAX_GAIN`.
const MAX_POSITION: f32 = 1.0 + 6.0206 / VOLUME_DB_RANGE;

/// Linear gain for a fader position, where 1.0 is unity.
///
/// Zero is true silence rather than the -60dB the formula would give: a fader
/// wound to the bottom is expected to be *off*.
pub fn gain_for(position: f32) -> f32 {
    let position = position.clamp(0.0, MAX_POSITION);
    if position <= 0.0 {
        return 0.0;
    }
    let db = VOLUME_DB_RANGE * (position - 1.0);
    (10.0f32.powf(db / 20.0)).min(MAX_GAIN)
}

/// State shared with the real-time callback.
///
/// **INVARIANT (DESIGN §3):** the callback may touch nothing but these atomics
/// and the ring buffer. No allocation, no locks, no syscalls, no panics.
pub struct Shared {
    /// Authoritative playback position, in device-rate frames, counted since
    /// the last flush rather than since the start of the current track.
    ///
    /// A gapless transition deliberately does *not* flush, so this keeps
    /// running across the join; `track_base` says where the current track
    /// began, and [`PlayerHandle::position`] is the difference.
    pub position_frames: AtomicU64,
    /// The value of `position_frames` at which the current track started.
    /// Zero except between a gapless transition and the next flush.
    pub track_base: AtomicU64,
    /// Times the ring ran dry while audio was still owed.
    pub underruns: AtomicU32,
    /// Frames missing at the most recent underrun, and the ring's capacity in
    /// frames. Recorded by the callback as plain atomic stores — the log is
    /// written elsewhere, because the callback may not touch a file.
    pub last_short_frames: AtomicU32,
    /// A `State`, as `u8`.
    pub state: AtomicU8,
    /// Linear gain, as `f32` bits. Derived from `position` — the callback
    /// reads it because it may not compute a power (DESIGN §3).
    pub volume: AtomicU32,
    /// Fader position, as `f32` bits. The value the user set, and the one the
    /// UI shows; the gain above is a function of it.
    pub position: AtomicU32,
    /// Bumped by the decoder to ask the callback to discard buffered audio.
    pub flush_seq: AtomicU64,
    /// Echoed back by the callback once it has discarded and repositioned.
    pub flush_ack: AtomicU64,
    /// Position to adopt when servicing the flush. Written before `flush_seq`.
    pub flush_position: AtomicU64,
    /// Set by the stream error callback for a *fatal* device failure; the
    /// decoder turns it into an event.
    pub device_lost: AtomicBool,
    /// The decoder has pushed the last of the current track and is waiting for
    /// the ring to play out.
    ///
    /// The ring going dry then is the *end of the track*, not a failure to
    /// keep up, and counting it makes the underrun figure meaningless: it
    /// becomes a count of tracks played.
    pub draining: AtomicBool,
    /// Set when the OS rerouted the stream to a different output (headphones
    /// plugged in, default device switched). Informational: playback carries
    /// on, so this must not be reported as an error.
    pub device_rerouted: AtomicBool,
    /// **Whether a departed output pauses playback** — #318. An
    /// [`OnDeviceGone`] as a flag, so a setting can change it while a stream
    /// runs; only the decoder reads it, never the callback.
    pub pause_when_gone: AtomicBool,

    /// **Which stream the frame counter is counting in** — #212.
    ///
    /// `position_frames` counts from the start of a stream, so it restarts at
    /// zero when a stream does, and a device disappearing is the likeliest
    /// hardware event in this design. Without a generation, a consumer
    /// extrapolating a position cannot tell a restart from a seek to the
    /// beginning: both look like the clock going backwards. `engine_generation`
    /// covers an engine restart and does not cover this one.
    ///
    /// Zero means no stream has run yet. Bumped once per stream, before the
    /// first render.
    pub stream_generation: AtomicU32,

    /// Seqlock sequence for the mapping below. Odd while it is being written.
    ///
    /// The four values are one fact — *this frame sounds at this instant* — and
    /// a reader that caught half of one and half of the next would compute a
    /// position that was never true. A seqlock rather than a mutex because the
    /// writer is the render step, which may not lock; the writer never waits
    /// and the reader retries.
    pub map_seq: AtomicU64,
    /// The frame index the mapping is about: the first frame of the buffer.
    pub map_frame: AtomicU64,
    /// When that frame reaches the output, in the stream's timebase.
    pub map_at_nanos: AtomicU64,
    /// The stream generation the two values above belong to. A mapping from a
    /// previous generation is refused, never reinterpreted.
    pub map_generation: AtomicU32,
    /// How far ahead of the sound the render step runs — the callback instant
    /// subtracted from the playback instant.
    ///
    /// **This is what makes a reported position the sounding one (I13).**
    /// `position_frames` counts frames *handed to the device*, which are not
    /// yet audible; without subtracting this, a playhead sits visibly ahead of
    /// what you can hear, and every beat-relative decision inherits the error.
    pub map_latency_nanos: AtomicU64,
}

/// Where the stream had got to, and when that will be audible — #212.
///
/// Read as one value or not at all, which is the whole point of publishing it
/// under a sequence: the frame and the instant are only meaningful together.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sounding {
    /// The stream this belongs to. A consumer that sees this change discards
    /// whatever it had extrapolated and asks again.
    pub generation: u32,
    /// The first frame of the buffer the mapping describes.
    pub frame: u64,
    /// When that frame reaches the output, in the stream's timebase.
    pub at_nanos: u64,
    /// How far ahead of the sound the render step is running.
    pub latency_nanos: u64,
}

impl Shared {
    /// The most recent frame-to-instant mapping, or `None` if no stream has
    /// published one yet.
    ///
    /// Retries a bounded number of times rather than spinning: the writer is
    /// the render step and never waits, so a torn read means it published
    /// again while we were looking, and the next attempt gets the newer value.
    /// An unbounded loop here would be the same mistake the callback is
    /// forbidden from making.
    ///
    /// **The bound is a backstop, not a tuning knob.** A device publishes once
    /// per buffer — a few milliseconds apart — so a reader wins on the first
    /// attempt essentially always. It was 8, which was enough for that and not
    /// enough for [`null::Pace::Fast`], where the renderer publishes in a tight
    /// loop and a reader could lose eight times in a row and report *no
    /// mapping* while one plainly existed. Losing 128 in a row means something
    /// far stranger than contention.
    pub fn sounding(&self) -> Option<Sounding> {
        for _ in 0..128 {
            let before = self.map_seq.load(Ordering::Acquire);
            if before == 0 || before & 1 == 1 {
                if before == 0 {
                    return None;
                }
                continue;
            }
            let frame = self.map_frame.load(Ordering::Relaxed);
            let at_nanos = self.map_at_nanos.load(Ordering::Relaxed);
            let latency_nanos = self.map_latency_nanos.load(Ordering::Relaxed);
            let generation = self.map_generation.load(Ordering::Relaxed);
            std::sync::atomic::fence(Ordering::Acquire);
            if self.map_seq.load(Ordering::Relaxed) == before {
                return Some(Sounding {
                    generation,
                    frame,
                    at_nanos,
                    latency_nanos,
                });
            }
        }
        None
    }

    /// Which stream is running. Zero before the first one starts.
    pub fn generation(&self) -> u32 {
        self.stream_generation.load(Ordering::Acquire)
    }

    /// **`pub(crate)`, so the control thread's tests can state a stream's state.**
    ///
    /// Every field here is already `pub` — the render step writes them from a
    /// callback that may not lock, so there is nothing to encapsulate. Only the
    /// constructor was private, which left a test wanting to describe "48,000 frames
    /// in, the track began at 47,616" with no way to say it except by borrowing a
    /// live player's `Shared` and racing its renderer.
    pub(crate) fn new() -> Self {
        Self {
            position_frames: AtomicU64::new(0),
            track_base: AtomicU64::new(0),
            underruns: AtomicU32::new(0),
            last_short_frames: AtomicU32::new(0),
            state: AtomicU8::new(State::Stopped as u8),
            volume: AtomicU32::new(1.0f32.to_bits()),
            position: AtomicU32::new(1.0f32.to_bits()),
            flush_seq: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            flush_position: AtomicU64::new(0),
            device_lost: AtomicBool::new(false),
            device_rerouted: AtomicBool::new(false),
            pause_when_gone: AtomicBool::new(true),
            draining: AtomicBool::new(false),
            stream_generation: AtomicU32::new(0),
            map_seq: AtomicU64::new(0),
            map_frame: AtomicU64::new(0),
            map_at_nanos: AtomicU64::new(0),
            map_generation: AtomicU32::new(0),
            map_latency_nanos: AtomicU64::new(0),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AudioError {
    #[error("no output device available")]
    NoDevice,
    #[error("device error: {0}")]
    Device(String),
    #[error("output stream: {0}")]
    Stream(#[from] cpal::Error),
}

/// Where rendered audio goes.
///
/// An enum rather than a trait, and deliberately: §12 asks for explicit typed
/// layers, the two cases are a closed set, and `cpal::Stream` is not `Send` on
/// every platform — which a `Box<dyn _>` would have to pretend about. The
/// renderer itself is shared either way; this only decides what drives it.
enum Sink {
    /// A real device. Dropping it stops the stream.
    ///
    /// Held and never read: the value *is* the effect, and dropping it is what
    /// stops rendering.
    Device(#[allow(dead_code, reason = "held to keep the stream alive")] cpal::Stream),
    /// A virtual clock, for headless runs and tests. Held for the same reason.
    Null(#[allow(dead_code, reason = "held to keep the renderer running")] null::Sink),
}

/// The handle the rest of the application uses to drive playback.
///
/// Dropping it shuts the decoder thread down and tears the stream down.
///
/// Not `Send`: it owns the cpal stream, which is not `Send` on every platform.
/// Create and use it from one thread; the decoder does the work anyway.
pub struct PlayerHandle {
    cmds: Sender<PlayerCmd>,
    events: Receiver<PlayerEvent>,
    shared: Arc<Shared>,
    device_rate: u32,
    /// Kept alive: dropping it stops rendering.
    _sink: Sink,
    decoder: Option<std::thread::JoinHandle<()>>,
}

impl PlayerHandle {
    /// Open the default output device and start the decoder thread.
    ///
    /// The device is opened **once**, at its preferred rate, and stays open for
    /// the life of the handle. Tracks whose rate differs are resampled — see
    /// the note on `device::preferred_config`.
    pub fn new(cfg: PlayerConfig) -> Result<Self, AudioError> {
        let (device, config) = device::preferred_config(&cfg)?;
        let device_rate = config.sample_rate;
        let device_channels = config.channels as usize;
        let route = route::Route::on(device::output_of(&device));

        let (mut producer, consumer) = ring::new(device_rate, device_channels);
        let shared = Arc::new(Shared::new());
        shared
            .pause_when_gone
            .store(cfg.on_device_gone == OnDeviceGone::Pause, Ordering::Relaxed);
        let position = cfg.volume.clamp(0.0, MAX_POSITION);
        shared.position.store(position.to_bits(), Ordering::Relaxed);
        shared
            .volume
            .store(gain_for(position).to_bits(), Ordering::Relaxed);

        let stream = device::build_stream(&device, config, consumer, Arc::clone(&shared))?;
        cpal::traits::StreamTrait::play(&stream)?;
        let sink = Sink::Device(stream);

        // Bounded: a UI that spams commands should never grow this without
        // limit. Sends are non-blocking (see `send`).
        let (cmd_tx, cmd_rx) = bounded::<PlayerCmd>(64);
        let (evt_tx, evt_rx) = bounded::<PlayerEvent>(64);

        let dec_shared = Arc::clone(&shared);
        let decoder = std::thread::Builder::new()
            .name("gmuse-decode".into())
            .spawn(move || {
                decode::run(
                    cmd_rx,
                    evt_tx,
                    &mut producer,
                    dec_shared,
                    device_rate,
                    device_channels,
                    cfg.resample,
                    route,
                );
            })
            .expect("failed to spawn decoder thread");

        Ok(Self {
            cmds: cmd_tx,
            events: evt_rx,
            shared,
            device_rate,
            _sink: sink,
            decoder: Some(decoder),
        })
    }

    /// A player with **no audio device**, rendering into nothing — #210.
    ///
    /// The same decoder, ring and render step as [`PlayerHandle::new`]; only
    /// what drives the render step differs. That is the whole value: a test
    /// that loads a track and watches the position advance is exercising the
    /// code that ships, not a mock of it.
    ///
    /// `pace` decides whether the virtual clock runs at wall speed or as fast
    /// as it can. A headless engine wants the first, because everything
    /// downstream reads positions as time. A test wants the second, because
    /// six minutes is six minutes.
    pub fn headless(cfg: PlayerConfig, pace: null::Pace) -> Result<Self, AudioError> {
        // A device would tell us these. Nothing is listening, so they are
        // only the units the rest of the player works in.
        const RATE: u32 = 48_000;
        const CHANNELS: usize = 2;
        const PERIOD: usize = 512;

        let (mut producer, consumer) = ring::new(RATE, CHANNELS);
        let shared = Arc::new(Shared::new());
        shared
            .pause_when_gone
            .store(cfg.on_device_gone == OnDeviceGone::Pause, Ordering::Relaxed);
        let position = cfg.volume.clamp(0.0, MAX_POSITION);
        shared.position.store(position.to_bits(), Ordering::Relaxed);
        shared
            .volume
            .store(gain_for(position).to_bits(), Ordering::Relaxed);

        let rt = device::Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        let sink = Sink::Null(null::Sink::start(rt, CHANNELS, RATE, PERIOD, pace));

        let (cmd_tx, cmd_rx) = bounded::<PlayerCmd>(64);
        let (evt_tx, evt_rx) = bounded::<PlayerEvent>(64);
        let dec_shared = Arc::clone(&shared);
        let decoder = std::thread::Builder::new()
            .name("gmuse-decode".into())
            .spawn(move || {
                decode::run(
                    cmd_rx,
                    evt_tx,
                    &mut producer,
                    dec_shared,
                    RATE,
                    CHANNELS,
                    cfg.resample,
                    // Nothing is listening, so no output can leave.
                    route::Route::on(None),
                );
            })
            .expect("failed to spawn decoder thread");

        Ok(Self {
            cmds: cmd_tx,
            events: evt_rx,
            shared,
            device_rate: RATE,
            _sink: sink,
            decoder: Some(decoder),
        })
    }

    /// Non-blocking send. A full queue means the decoder is wedged; dropping
    /// the command is better than blocking a render loop (DESIGN §3).
    fn send(&self, cmd: PlayerCmd) {
        match self.cmds.try_send(cmd) {
            Ok(()) | Err(TrySendError::Disconnected(_)) => {}
            Err(TrySendError::Full(dropped)) => {
                debug_assert!(false, "player command queue full, dropped {dropped:?}");
            }
        }
    }

    pub fn load(&self, source: TrackSource) {
        self.send(PlayerCmd::Load(source));
    }
    pub fn play(&self) {
        self.send(PlayerCmd::Play);
    }
    pub fn pause(&self) {
        self.send(PlayerCmd::Pause);
    }
    pub fn toggle(&self) {
        self.send(PlayerCmd::Toggle);
    }
    /// **Not reachable from a view, and kept on purpose** — #229.
    ///
    /// `stop` is gone as a *verb*: D-10 argues it is pause plus a return to the
    /// reference, which is cue, and the bottom row now spends `v` on next. What
    /// stays is this, because stop is not only that. It tears the pipeline down
    /// — clears the outbox, drops the answer to `NeedNext`, cancels draining —
    /// which is the **queue-level** condition #224's O-4 named: the queue not
    /// advancing, which a deck has no concept of. The engine reaches it on its
    /// own at the end of a programme and when a device is lost; this is the door
    /// for when the queue itself is what stops.
    pub fn stop(&self) {
        self.send(PlayerCmd::Stop);
    }

    pub fn cue(&self) {
        self.send(PlayerCmd::Cue);
    }

    pub fn cue_preview(&self) {
        self.send(PlayerCmd::CuePreview);
    }
    pub fn seek(&self, to: SeekTo) {
        self.send(PlayerCmd::Seek(to));
    }
    /// Answer a [`PlayerEvent::NeedNext`]. Ignored at any other time.
    pub fn set_next(&self, next: Option<TrackSource>) {
        self.send(PlayerCmd::SetNext(next));
    }

    /// Events from the decoder. Drain with `try_recv` from a render loop.
    pub fn events(&self) -> &Receiver<PlayerEvent> {
        &self.events
    }

    pub fn state(&self) -> State {
        State::from_u8(self.shared.state.load(Ordering::Relaxed))
    }

    /// Current playback position.
    ///
    /// Derived from the frame counter the callback maintains, not from a timer,
    /// so it cannot drift from what is actually coming out of the speakers.
    /// A seek repositions the counter as part of the flush handshake, so this
    /// never reports a stale position afterwards.
    /// How far into the current track the sound has got — **what you can
    /// hear, not what has been handed over** (#212, I13).
    ///
    /// `position_frames` counts frames written to the device, and those are
    /// not audible yet: they sit in the device's buffer for the output
    /// latency, which the render step publishes from the gap between its
    /// callback and playback instants. Subtracting it is the difference
    /// between a playhead that tracks the music and one that runs visibly
    /// ahead of it.
    ///
    /// Saturating at zero, which is what the first buffer of a track wants:
    /// the sound has not started, so the position is the start rather than a
    /// negative time wrapped into an enormous one.
    pub fn position(&self) -> Duration {
        let frames = self
            .shared
            .position_frames
            .load(Ordering::Relaxed)
            .saturating_sub(self.shared.track_base.load(Ordering::Relaxed));
        let handed = Duration::from_secs_f64(frames as f64 / self.device_rate as f64);
        handed.saturating_sub(self.output_latency())
    }

    /// How far ahead of the sound the render step is running. Zero until a
    /// stream has published a mapping.
    pub fn output_latency(&self) -> Duration {
        match self.shared.sounding() {
            Some(map) if map.generation == self.shared.generation() => {
                Duration::from_nanos(map.latency_nanos)
            }
            // **A mapping from a previous stream is refused, not reinterpreted**
            // (#212). Its latency belonged to a device that is no longer the
            // one playing, and guessing that it still applies is exactly the
            // silent wrongness the generation exists to prevent.
            _ => Duration::ZERO,
        }
    }

    /// The frame-to-instant mapping the running stream last published, or
    /// `None` if none has or it belongs to a stream that has been replaced.
    pub fn sounding(&self) -> Option<Sounding> {
        self.shared
            .sounding()
            .filter(|map| map.generation == self.shared.generation())
    }

    /// Which stream is running. Bumped once per stream; zero before the first.
    /// The atomics the render step publishes into.
    ///
    /// **Handed out so another thread can read them** — §1's feedback path
    /// needs the control thread to see state, position and the frame-to-instant
    /// mapping without asking the decoder anything. Every field is an atomic
    /// and `Shared` is already `pub`; what this adds is reaching them from a
    /// thread that does not own the handle, which is what makes a `playback`
    /// event possible at all.
    pub fn shared(&self) -> Arc<Shared> {
        Arc::clone(&self.shared)
    }

    pub fn stream_generation(&self) -> u32 {
        self.shared.generation()
    }

    pub fn underruns(&self) -> u32 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    /// Frames missing at the most recent underrun — how *badly* it starved,
    /// which a bare count cannot tell you.
    pub fn last_short_frames(&self) -> u32 {
        self.shared.last_short_frames.load(Ordering::Relaxed)
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// Fader position, where 1.0 is unity. This is the value to show and to
    /// step, not the gain — see [`gain_for`].
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.position.load(Ordering::Relaxed))
    }

    /// Set the fader position, clamped to a sane range.
    ///
    /// The mapping to gain happens here, so the callback only ever multiplies
    /// by a number it was handed. Applied in the callback rather than the
    /// decoder, so a change is audible immediately instead of after the ~1s
    /// already sitting in the ring.
    pub fn set_volume(&self, position: f32) {
        let position = position.clamp(0.0, MAX_POSITION);
        self.shared
            .position
            .store(position.to_bits(), Ordering::Relaxed);
        self.shared
            .volume
            .store(gain_for(position).to_bits(), Ordering::Relaxed);
    }

    /// What a departed output does to playback from now on — #318.
    pub fn set_on_device_gone(&self, to: OnDeviceGone) {
        self.shared
            .pause_when_gone
            .store(to == OnDeviceGone::Pause, Ordering::Relaxed);
    }

    /// What a departed output does to playback now.
    pub fn on_device_gone(&self) -> OnDeviceGone {
        if self.shared.pause_when_gone.load(Ordering::Relaxed) {
            OnDeviceGone::Pause
        } else {
            OnDeviceGone::Continue
        }
    }
}

impl Drop for PlayerHandle {
    fn drop(&mut self) {
        let _ = self.cmds.send(PlayerCmd::Shutdown);
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}
/// **Cue means two things and the state picks** — #229, D-14.
///
/// Running returns to the reference; not running marks one. The point of
/// asserting it as a table is that the two are exhaustive over the states
/// that can reach it, so a fourth behaviour cannot appear without this
/// changing.
#[test]
fn cue_returns_while_running_and_marks_while_not() {
    assert_eq!(cued(State::Playing, true), Cueing::ReturnAndPause);
    assert_eq!(cued(State::Paused, true), Cueing::MarkHere);
    assert_eq!(cued(State::Stopped, true), Cueing::MarkHere);
}

/// **With nothing loaded, cue does nothing at all** — #229.
///
/// There is no playhead to mark and nowhere to return to. Checked over
/// every state rather than one, because "not loaded" has to win against all
/// of them — a `cued` that read the state first would still pass a single
/// case.
///
/// The count is the instrument: an empty `State::ALL` would satisfy a
/// for-loop of assertions without executing one.
#[test]
fn cue_does_nothing_with_nothing_loaded() {
    let states = [State::Stopped, State::Playing, State::Paused];
    assert!(states.len() >= 3, "not every state was tried");
    for state in states {
        assert_eq!(
            cued(state, false),
            Cueing::Nothing,
            "cue acted from {state:?} with nothing loaded"
        );
    }
}

/// **Marking is idempotent where it matters** — #229.
///
/// Cue twice from paused and the second press marks the same frame, because
/// the playhead has not moved. That is what makes "return, then press again"
/// safe on a CDJ, and it falls out of the rule rather than needing a guard —
/// so this is the assertion that the rule *keeps* falling out that way.
#[test]
fn cue_from_paused_is_the_same_answer_twice() {
    assert_eq!(cued(State::Paused, true), cued(State::Paused, true));
    assert_eq!(cued(State::Paused, true), Cueing::MarkHere);
}

#[cfg(test)]
mod tests {
    /// **Play and Pause are idempotent, from every state** — #225, acceptance criterion 3.
    ///
    /// Over every initial state and both load conditions, because the argument for carrying
    /// three verbs rather than one is exactly that two of them are no-ops when the stream is
    /// already where they would put it. A held key without the kitty keyboard protocol arrives
    /// as repeated presses; `Play` twice must be one change, where a toggle would flutter.
    #[test]
    fn play_and_pause_are_idempotent_from_every_state() {
        use super::{State, Verb, transported};

        let states = [State::Stopped, State::Playing, State::Paused];
        let mut moved = 0;
        for loaded in [false, true] {
            for start in states {
                for verb in [Verb::Play, Verb::Pause] {
                    let once = transported(start, verb, loaded);
                    let twice = transported(once, verb, loaded);
                    assert_eq!(
                        once, twice,
                        "{verb:?} from {start:?} (loaded {loaded}) moved again on the second"
                    );
                    moved += usize::from(once != start);
                }
            }
        }
        // **The instrument.** A `transported` that returned its input would satisfy every
        // assertion above, and idempotence would be proven of a function that does nothing.
        assert!(
            moved >= 2,
            "{moved} of the twelve cases changed anything, so this proves nothing"
        );
    }

    /// **Toggle twice is one change** — #225, acceptance criterion 4, the D-8 regression.
    ///
    /// The race the sender-resolved toggle had: two presses arriving faster than the snapshot
    /// refresh both read `paused`, both decide "so play", and the second undoes nothing. Here
    /// the engine resolves it, so the second press sees the first one's result.
    #[test]
    fn toggle_twice_returns_to_where_it_started() {
        use super::{State, Verb, transported};

        for start in [State::Playing, State::Paused] {
            let once = transported(start, Verb::Toggle, true);
            assert_ne!(once, start, "toggle from {start:?} changed nothing");
            assert_eq!(
                transported(once, Verb::Toggle, true),
                start,
                "toggling twice from {start:?} did not come back"
            );
        }
    }

    /// **Nothing starts without a track**, which is what makes `Play` safe to send at anything.
    #[test]
    fn play_does_nothing_with_nothing_loaded() {
        use super::{State, Verb, transported};

        for start in [State::Stopped, State::Paused] {
            assert_eq!(transported(start, Verb::Play, false), start);
        }
        // And with one, it starts — or the assertion above is about a function that never
        // starts anything.
        assert_eq!(
            transported(State::Paused, Verb::Play, true),
            State::Playing,
            "a loaded track did not start"
        );
    }

    use super::*;

    /// A real WAV, so there is audio to decode rather than a stub to mock.
    fn wav(path: &std::path::Path, frames: usize) {
        use std::io::Write;
        let bytes = 4 * frames;
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(b"RIFF").unwrap();
        f.write_all(&((36 + bytes) as u32).to_le_bytes()).unwrap();
        f.write_all(b"WAVEfmt ").unwrap();
        f.write_all(&16u32.to_le_bytes()).unwrap();
        f.write_all(&1u16.to_le_bytes()).unwrap();
        f.write_all(&2u16.to_le_bytes()).unwrap();
        f.write_all(&48_000u32.to_le_bytes()).unwrap();
        f.write_all(&192_000u32.to_le_bytes()).unwrap();
        f.write_all(&4u16.to_le_bytes()).unwrap();
        f.write_all(&16u16.to_le_bytes()).unwrap();
        f.write_all(b"data").unwrap();
        f.write_all(&(bytes as u32).to_le_bytes()).unwrap();
        for n in 0..frames {
            let v = ((n as f32 * 0.01).sin() * 8000.0) as i16;
            f.write_all(&v.to_le_bytes()).unwrap();
            f.write_all(&v.to_le_bytes()).unwrap();
        }
    }

    /// **Underruns are zero across a playback run** — the standing measurement
    /// rule, and until #210 there was no way to assert it without a sound card.
    ///
    /// At `RealTime` the virtual clock asks for a buffer every buffer's worth
    /// of wall time, exactly as a device does, so the decoder has the same
    /// deadline it has in production. A starved ring here means a starved ring
    /// there.
    ///
    /// (`Pace::Fast` would underrun by construction: the renderer outruns the
    /// decoder on purpose, which is what makes it fast.)
    #[test]
    fn a_real_time_run_starves_nothing() {
        let dir = std::env::temp_dir().join("gmuse-null-underruns");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("tone.wav");
        wav(&track, 48_000);

        let player = PlayerHandle::headless(
            PlayerConfig {
                device: None,
                rate: None,
                resample: resample::Quality::Fast,
                volume: 1.0,
                on_device_gone: Default::default(),
            },
            null::Pace::RealTime,
        )
        .expect("a player with no device");

        player.load(TrackSource::plain(&track));
        player.play();

        // Half a second of the one-second tone: long enough for the ring to
        // have been drained many times, short enough not to dominate the suite.
        let until = std::time::Instant::now() + Duration::from_millis(500);
        while std::time::Instant::now() < until {
            std::thread::sleep(Duration::from_millis(20));
        }

        assert!(
            player.position() > Duration::ZERO,
            "nothing played, so nothing was measured"
        );
        assert_eq!(
            player.underruns(),
            0,
            "the ring starved during ordinary playback"
        );
    }

    /// **A track plays through with no audio device** — #210, and the reason
    /// everything after it can be tested at all.
    ///
    /// Nothing here is a mock. The decoder, the ring and the render step are
    /// the ones that ship; only what drives the render step differs, so a
    /// position that advances is the real counter being advanced by the real
    /// renderer draining the real ring.
    #[test]
    fn a_track_plays_through_with_no_device() {
        let dir = std::env::temp_dir().join("gmuse-null-audio");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let track = dir.join("tone.wav");
        // A quarter second at 48k, which is several hundred buffers of 512.
        wav(&track, 12_000);

        let player = PlayerHandle::headless(
            PlayerConfig {
                device: None,
                rate: None,
                resample: resample::Quality::Fast,
                volume: 1.0,
                on_device_gone: Default::default(),
            },
            null::Pace::Fast,
        )
        .expect("a player with no device");

        player.load(TrackSource::plain(&track));
        player.play();

        // The renderer runs flat out, so this is bounded by the decoder rather
        // than by the audio's duration.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while player.position() == Duration::ZERO && std::time::Instant::now() < deadline {
            std::thread::yield_now();
        }

        assert!(
            player.position() > Duration::ZERO,
            "nothing was rendered, so the position never moved"
        );
        assert_eq!(player.state(), State::Playing);
    }

    fn db(gain: f32) -> f32 {
        20.0 * gain.log10()
    }

    /// The property the whole change exists for: a step of the fader moves the
    /// level by the same number of decibels wherever it is taken.
    ///
    /// Under the old raw-amplitude mapping this failed badly — a 5% step was
    /// 0.45dB at the top and 6dB near the bottom — and that spread *is* the
    /// bug. Testing the curve's shape at a few points would not have caught
    /// it; testing the step does.
    #[test]
    fn every_step_of_the_fader_is_the_same_number_of_decibels() {
        const STEP: f32 = 0.05;
        let mut position = STEP;
        let mut steps = Vec::new();
        while position + STEP <= 1.0 + 1e-6 {
            let change = db(gain_for(position + STEP)) - db(gain_for(position));
            steps.push(change);
            position += STEP;
        }
        assert!(steps.len() > 15, "not enough of the range covered");

        let expected = VOLUME_DB_RANGE * STEP;
        for change in &steps {
            assert!(
                (change - expected).abs() < 0.01,
                "a {STEP} step moved {change:.3}dB, expected {expected:.3}dB; \
                 steps were {steps:?}"
            );
        }
    }

    /// 5% of a 60dB range is 3dB, which is both well above the ~1dB a listener
    /// can notice and small enough to tune with. Pinned because the range and
    /// the default step have to be chosen together to land here.
    #[test]
    fn the_default_step_is_three_decibels() {
        let change = db(gain_for(1.0)) - db(gain_for(0.95));
        assert!((change - 3.0).abs() < 0.01, "a 5% step moved {change:.3}dB");
    }

    #[test]
    fn the_fader_spans_sixty_decibels_below_unity() {
        assert!(
            (gain_for(1.0) - 1.0).abs() < 1e-6,
            "100% must be unity gain"
        );
        assert!(
            (db(gain_for(f32::EPSILON)) + VOLUME_DB_RANGE).abs() < 0.01,
            "the bottom of the fader should be -{VOLUME_DB_RANGE}dB"
        );
    }

    /// A fader wound to the bottom is off, not merely very quiet. The formula
    /// alone would give -60dB, which is audible in a quiet room.
    #[test]
    fn zero_is_true_silence() {
        assert_eq!(gain_for(0.0), 0.0);
        assert_eq!(gain_for(-1.0), 0.0);
    }

    /// The change alters the shape of the control, not how loud it can get:
    /// the ceiling is the +6dB it always was.
    #[test]
    fn the_ceiling_is_unchanged_at_plus_six_decibels() {
        assert!((gain_for(MAX_POSITION) - MAX_GAIN).abs() < 1e-3);
        assert!((db(gain_for(MAX_POSITION)) - 6.02).abs() < 0.01);
        assert_eq!(gain_for(99.0), MAX_GAIN, "the clamp must hold");
    }

    /// Half the fader is a large cut, not the -6dB a raw multiplier gave.
    #[test]
    fn half_the_fader_is_half_the_range() {
        assert!((db(gain_for(0.5)) + 30.0).abs() < 0.01);
    }
}
