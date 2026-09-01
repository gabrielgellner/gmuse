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
mod device;
pub mod resample;
mod ring;

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
    /// *below* the store and is deliberately ignorant of `TrackId`. A path and
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
pub(crate) struct Shared {
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
}

impl Shared {
    fn new() -> Self {
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
            draining: AtomicBool::new(false),
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
    /// Kept alive: dropping the stream stops the device.
    _stream: cpal::Stream,
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

        let (mut producer, consumer) = ring::new(device_rate, device_channels);
        let shared = Arc::new(Shared::new());
        let position = cfg.volume.clamp(0.0, MAX_POSITION);
        shared.position.store(position.to_bits(), Ordering::Relaxed);
        shared
            .volume
            .store(gain_for(position).to_bits(), Ordering::Relaxed);

        let stream = device::build_stream(&device, config, consumer, Arc::clone(&shared))?;
        cpal::traits::StreamTrait::play(&stream)?;

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
                );
            })
            .expect("failed to spawn decoder thread");

        Ok(Self {
            cmds: cmd_tx,
            events: evt_rx,
            shared,
            device_rate,
            _stream: stream,
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
    pub fn stop(&self) {
        self.send(PlayerCmd::Stop);
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
    pub fn position(&self) -> Duration {
        let frames = self
            .shared
            .position_frames
            .load(Ordering::Relaxed)
            .saturating_sub(self.shared.track_base.load(Ordering::Relaxed));
        Duration::from_secs_f64(frames as f64 / self.device_rate as f64)
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
}

impl Drop for PlayerHandle {
    fn drop(&mut self) {
        let _ = self.cmds.send(PlayerCmd::Shutdown);
        if let Some(handle) = self.decoder.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
