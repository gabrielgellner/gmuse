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

/// Commands sent to the decoder thread.
#[derive(Debug, Clone)]
pub enum PlayerCmd {
    Load(PathBuf),
    Play,
    Pause,
    Toggle,
    Stop,
    Seek(SeekTo),
    Shutdown,
}

/// Things the decoder thread reports back.
///
/// Position is deliberately *not* an event: it comes from an atomic the RT
/// callback updates, so the UI can read it at whatever rate it renders rather
/// than being driven by a message per frame.
#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Loaded {
        path: PathBuf,
        duration: Option<Duration>,
        source_rate: u32,
        channels: usize,
        /// True when the file's rate differs from the device's and is being converted.
        resampling: bool,
    },
    /// The loaded track played to its end.
    Finished,
    /// Decoding or demuxing failed; the track was abandoned.
    Error(String),
    /// The output device went away (unplugged, or the stream errored).
    DeviceLost(String),
    /// The OS moved playback to a different output and the stream survived.
    /// Worth telling the user; not worth treating as a failure.
    DeviceChanged,
}

/// State shared with the real-time callback.
///
/// **INVARIANT (DESIGN §3):** the callback may touch nothing but these atomics
/// and the ring buffer. No allocation, no locks, no syscalls, no panics.
pub(crate) struct Shared {
    /// Authoritative playback position, in device-rate frames.
    pub position_frames: AtomicU64,
    /// Times the ring ran dry while audio was still owed.
    pub underruns: AtomicU32,
    /// A `State`, as `u8`.
    pub state: AtomicU8,
    /// Linear gain, as `f32` bits.
    pub volume: AtomicU32,
    /// Bumped by the decoder to ask the callback to discard buffered audio.
    pub flush_seq: AtomicU64,
    /// Echoed back by the callback once it has discarded and repositioned.
    pub flush_ack: AtomicU64,
    /// Position to adopt when servicing the flush. Written before `flush_seq`.
    pub flush_position: AtomicU64,
    /// Set by the stream error callback for a *fatal* device failure; the
    /// decoder turns it into an event.
    pub device_lost: AtomicBool,
    /// Set when the OS rerouted the stream to a different output (headphones
    /// plugged in, default device switched). Informational: playback carries
    /// on, so this must not be reported as an error.
    pub device_rerouted: AtomicBool,
}

impl Shared {
    fn new() -> Self {
        Self {
            position_frames: AtomicU64::new(0),
            underruns: AtomicU32::new(0),
            state: AtomicU8::new(State::Stopped as u8),
            volume: AtomicU32::new(1.0f32.to_bits()),
            flush_seq: AtomicU64::new(0),
            flush_ack: AtomicU64::new(0),
            flush_position: AtomicU64::new(0),
            device_lost: AtomicBool::new(false),
            device_rerouted: AtomicBool::new(false),
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
        shared
            .volume
            .store(cfg.volume.clamp(0.0, 2.0).to_bits(), Ordering::Relaxed);

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

    pub fn load(&self, path: impl Into<PathBuf>) {
        self.send(PlayerCmd::Load(path.into()));
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
        let frames = self.shared.position_frames.load(Ordering::Relaxed);
        Duration::from_secs_f64(frames as f64 / self.device_rate as f64)
    }

    pub fn underruns(&self) -> u32 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    pub fn device_rate(&self) -> u32 {
        self.device_rate
    }

    /// Linear gain. 1.0 is unity; values above 1.0 will clip.
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.shared.volume.load(Ordering::Relaxed))
    }

    /// Set linear gain, clamped to a sane range.
    ///
    /// Applied in the callback rather than the decoder, so a change is audible
    /// immediately instead of after the ~1s already sitting in the ring.
    pub fn set_volume(&self, gain: f32) {
        let gain = gain.clamp(0.0, 2.0);
        self.shared.volume.store(gain.to_bits(), Ordering::Relaxed);
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
