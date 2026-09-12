//! Output device selection and the real-time callback.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{ErrorKind, SampleFormat, StreamConfig, SupportedBufferSize};

use super::{AudioError, PlayerConfig, Shared, State};

/// Pick the output device and the config to hold it at for the whole session.
///
/// DESIGN §11.5 settled on always resampling rather than reconfiguring the
/// device per track, and a persistent stream is what makes that work: the
/// device is opened once at its own preferred rate and never reopened, so
/// changing tracks — including between a 44.1k MP3 and a 48k Opus — never
/// touches the device. Reopening it per track would put an audible gap in
/// exactly the place §11.1 needs a gapless transition.
///
pub(super) fn preferred_config(
    cfg: &PlayerConfig,
) -> Result<(cpal::Device, StreamConfig), AudioError> {
    let host = cpal::default_host();

    let device = match cfg.device.as_deref() {
        Some(want) => host
            .output_devices()
            .map_err(|e| AudioError::Device(e.to_string()))?
            .find(|d| {
                d.id()
                    .map(|i| i.to_string().contains(want))
                    .unwrap_or(false)
            })
            .ok_or(AudioError::NoDevice)?,
        None => host.default_output_device().ok_or(AudioError::NoDevice)?,
    };

    let default = device
        .default_output_config()
        .map_err(|e| AudioError::Device(e.to_string()))?;

    // We only ever write f32. Every host cpal supports offers it.
    if default.sample_format() != SampleFormat::F32 {
        return Err(AudioError::Device(format!(
            "device default config is {:?}, only f32 is supported",
            default.sample_format()
        )));
    }

    let mut config: StreamConfig = default.into();

    // --rate forces a rate, to measure conversion cost in isolation.
    if let Some(forced) = cfg.rate
        && let Ok(ranges) = device.supported_output_configs()
        && ranges.into_iter().any(|c| {
            c.sample_format() == SampleFormat::F32
                && c.channels() == default.channels()
                && c.min_sample_rate() <= forced
                && forced <= c.max_sample_rate()
        })
    {
        config.sample_rate = forced;
    }

    // A modest period keeps latency sane; the ring absorbs decode jitter.
    if let SupportedBufferSize::Range { min, max } = default.buffer_size() {
        config.buffer_size = cpal::BufferSize::Fixed((*min).max(512).min(*max));
    }

    Ok((device, config))
}

/// Time spent ramping between silence and full level on a transport change.
///
/// Long enough to remove the discontinuity, short enough that pausing still
/// feels instant: 8ms is under the ~20ms at which a fade starts to read *as* a
/// fade.
const FADE: Duration = Duration::from_millis(8);

/// A declick ramp for transport changes.
///
/// Cutting straight to silence on pause leaves a step discontinuity, which is
/// broadband by definition — that is the pop, and rapid pause/resume makes one
/// per press. Ramping over a few milliseconds removes the discontinuity in
/// both value and slope.
///
/// Also applied on seek, where the audio either side is unrelated — see
/// [`Fade::cut`].
#[derive(Debug)]
pub(super) struct Fade {
    /// Current level, 0.0..=1.0, in *linear ramp position*.
    position: f32,
    /// Fraction of the ramp covered by one frame.
    step: f32,
}

impl Fade {
    pub(super) fn new(sample_rate: u32) -> Self {
        // seconds * frames-per-second = frames.
        let frames = FADE.as_secs_f32() * sample_rate as f32;
        Self {
            position: 0.0,
            step: 1.0 / frames.max(1.0),
        }
    }

    /// Silent and not moving, so the caller can skip the ring entirely.
    pub(super) fn is_silent(&self, target: f32) -> bool {
        self.position <= 0.0 && target <= 0.0
    }

    /// Drop straight to silence, for a seek — the audio either side of a seek
    /// is unrelated, so it should fade in rather than continue.
    pub(super) fn cut(&mut self) {
        self.position = 0.0;
    }

    /// Apply the ramp in place, advancing toward `target` (0.0 or 1.0).
    ///
    /// `volume` is folded in here so the buffer is only walked once.
    pub(super) fn apply(&mut self, out: &mut [f32], channels: usize, target: f32, volume: f32) {
        for frame in out.chunks_mut(channels.max(1)) {
            if self.position < target {
                self.position = (self.position + self.step).min(target);
            } else if self.position > target {
                self.position = (self.position - self.step).max(target);
            }
            let gain = raised_cosine(self.position) * volume;
            for sample in frame.iter_mut() {
                *sample *= gain;
            }
        }
    }

    /// Ramp down to silence across exactly this buffer, for a ring that ran
    /// dry.
    ///
    /// The normal ramp needs a fixed number of frames; a starved callback may
    /// have far fewer left. Scaling the step to what is actually available
    /// still lands on zero at the last real sample, so the gap is entered
    /// smoothly instead of stepping off a cliff.
    pub(super) fn fade_to_silence(&mut self, out: &mut [f32], channels: usize, volume: f32) {
        let channels = channels.max(1);
        let frames = (out.len() / channels).max(1);
        let step = self.position / frames as f32;
        for frame in out.chunks_mut(channels) {
            self.position = (self.position - step).max(0.0);
            let gain = raised_cosine(self.position) * volume;
            for sample in frame.iter_mut() {
                *sample *= gain;
            }
        }
        self.position = 0.0;
    }
}

/// Raised cosine over `0.0..=1.0`. Continuous in slope at both ends, where a
/// linear ramp still has a corner.
fn raised_cosine(position: f32) -> f32 {
    (1.0 - (position * std::f32::consts::PI).cos()) * 0.5
}

/// Produce one callback's worth of output from `got` frames of real audio.
///
/// Split out from the stream closure so the starvation path can be tested:
/// that is where the discontinuity lived, and it is invisible to a test that
/// only drives [`Fade`] directly.
///
/// Returns whether the ring ran dry.
pub(super) fn fill_output(
    out: &mut [f32],
    got: usize,
    channels: usize,
    target: f32,
    volume: f32,
    fade: &mut Fade,
) -> bool {
    if got < out.len() {
        // Starved. Ramp the audio we do have down into the gap rather than
        // cutting, and leave the fade silent so playback ramps back up when
        // the ring recovers — otherwise every underrun costs two clicks, one
        // going quiet and one coming back.
        fade.fade_to_silence(&mut out[..got], channels, volume);
        out[got..].fill(0.0);
        true
    } else {
        fade.apply(out, channels, target, volume);
        false
    }
}

/// Build the output stream.
///
/// **INVARIANT (DESIGN §3).** Inside the callback: no allocation, no locking,
/// no syscalls, no panics, no unbounded loops. It reads the ring, applies gain,
/// writes the output slice, and updates atomics. Nothing else, ever.
/// Everything the render step owns between callbacks.
///
/// Callback-local state, held here rather than captured in a closure so the
/// step can be driven by something other than cpal — a virtual clock, in a
/// test with no audio device.
/// When this buffer was computed, and when it will be heard — #212.
///
/// **Two instants, and the second is the one that matters.** A device supplies
/// both; the gap between them is the output latency, which is what separates
/// the frames handed over from the frames coming out of the speaker.
///
/// Plain integers rather than `cpal::OutputCallbackInfo`, for the same reason
/// [`process`] takes two numbers rather than a `StreamConfig`: naming cpal here
/// would put it back on the headless path, and the null renderer has a virtual
/// clock that is every bit as real to the code under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderClock {
    /// When the render step was invoked, in the stream's timebase.
    pub callback_nanos: u64,
    /// When the audio written here reaches the output.
    pub playback_nanos: u64,
}

impl RenderClock {
    /// How far ahead of the sound this buffer is being computed.
    ///
    /// Saturating: a backend whose playback instant precedes its callback
    /// instant is reporting nonsense, and a wrapped subtraction would turn
    /// that into an enormous latency rather than none.
    pub fn latency_nanos(&self) -> u64 {
        self.playback_nanos.saturating_sub(self.callback_nanos)
    }
}

pub struct Rt {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    /// The last flush the render step has serviced. Not an atomic: only this
    /// step reads or writes it.
    serviced: u64,
    fade: Fade,
    /// The stream this renderer belongs to, captured when it was built.
    ///
    /// Held rather than read from `shared` each buffer so that a renderer
    /// outliving its stream publishes its *own* generation and is therefore
    /// detectable, instead of silently adopting the new one.
    generation: u32,
}

impl Rt {
    /// **The two numbers, not a cpal config.** A device supplies them, but
    /// nothing about rendering needs to know that — and a signature naming
    /// `StreamConfig` would put cpal back on the headless path.
    pub fn new(
        consumer: rtrb::Consumer<f32>,
        shared: Arc<Shared>,
        channels: usize,
        rate: u32,
    ) -> Rt {
        // **The stream begins here, not at the first buffer.** Bumping on
        // construction means a consumer that reads the generation before any
        // audio has been rendered still sees the stream it is about to hear,
        // and the mapping this renderer publishes can never carry a generation
        // nobody has been told about.
        let generation = shared
            .stream_generation
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel)
            .wrapping_add(1);
        Rt {
            consumer,
            channels,
            fade: Fade::new(rate),
            shared,
            serviced: 0,
            generation,
        }
    }

    /// Which stream this renderer is rendering.
    pub fn generation(&self) -> u32 {
        self.generation
    }

    /// Publish *this frame sounds at this instant*, as one fact.
    ///
    /// **Seqlock, written from the render step.** The sequence goes odd, the
    /// four values are stored, the sequence goes even; a reader that sees an
    /// odd sequence or a changed one retries. No allocation, no lock, no
    /// syscall, and no loop — which is what the callback's rules require.
    fn publish(&self, frame: u64, clock: RenderClock) {
        use std::sync::atomic::{Ordering, fence};
        let odd = self.shared.map_seq.load(Ordering::Relaxed).wrapping_add(1);
        self.shared.map_seq.store(odd, Ordering::Relaxed);
        // The odd sequence must be visible before the values it guards.
        fence(Ordering::Release);
        self.shared.map_frame.store(frame, Ordering::Relaxed);
        self.shared
            .map_at_nanos
            .store(clock.playback_nanos, Ordering::Relaxed);
        self.shared
            .map_latency_nanos
            .store(clock.latency_nanos(), Ordering::Relaxed);
        self.shared
            .map_generation
            .store(self.generation, Ordering::Relaxed);
        self.shared
            .map_seq
            .store(odd.wrapping_add(1), Ordering::Release);
    }
}

/// Render one buffer.
///
/// **INVARIANT (DESIGN §3).** No allocation, no locking, no syscalls, no
/// panics, no unbounded loops. It reads the ring, applies gain, writes the
/// output slice, and updates atomics. Nothing else, ever.
///
/// A free function taking `&mut Rt` rather than a closure, so the same code
/// runs under cpal and under a virtual clock. Nothing in the signature names
/// cpal, which is what makes the headless path the *same* renderer rather
/// than a second one that drifts.
pub fn process(rt: &mut Rt, out: &mut [f32], clock: RenderClock) {
    // Seek/stop handshake: discard buffered audio and adopt the new
    // position. `read_chunk(..).commit_all()` moves an index; it does
    // not copy or free, so this is safe in the RT context.
    let requested = rt.shared.flush_seq.load(Ordering::Acquire);
    if requested != rt.serviced {
        let stale = rt.consumer.slots();
        if stale > 0
            && let Ok(chunk) = rt.consumer.read_chunk(stale)
        {
            chunk.commit_all();
        }
        rt.shared.position_frames.store(
            rt.shared.flush_position.load(Ordering::Relaxed),
            Ordering::Relaxed,
        );
        rt.serviced = requested;
        rt.shared.flush_ack.store(requested, Ordering::Release);
        // Audio either side of a seek is unrelated; fade in rather
        // than splice, or the seek itself clicks.
        rt.fade.cut();
        out.fill(0.0);
        // **Published on every path, including the silent ones.** A paused or
        // seeking stream still calls back, and a mapping that only moved while
        // audio flowed would leave a consumer extrapolating from an instant
        // that is seconds old the moment anything stops.
        rt.publish(rt.shared.position_frames.load(Ordering::Relaxed), clock);
        return;
    }

    // Paused and stopped both hold the device open and emit silence.
    // Stopping the stream instead would risk a click on resume, and
    // would drop the ring contents we want to resume from.
    let target = if State::from_u8(rt.shared.state.load(Ordering::Relaxed)) == State::Playing {
        1.0
    } else {
        0.0
    };

    // Fully faded out: emit silence without touching the ring, so a
    // pause holds its place rather than draining.
    if rt.fade.is_silent(target) {
        out.fill(0.0);
        rt.publish(rt.shared.position_frames.load(Ordering::Relaxed), clock);
        return;
    }

    // **The mapping is about the first frame of this buffer**, which is the
    // one that sounds at the playback instant. Taken before the counter moves;
    // taking it after would name a frame that has not been handed over yet.
    let at_frame = rt.shared.position_frames.load(Ordering::Relaxed);
    rt.publish(at_frame, clock);

    let want = out.len();
    let got = match rt.consumer.read_chunk(want.min(rt.consumer.slots())) {
        Ok(chunk) => {
            let n = chunk.len();
            let (a, b) = chunk.as_slices();
            out[..a.len()].copy_from_slice(a);
            out[a.len()..a.len() + b.len()].copy_from_slice(b);
            chunk.commit_all();
            n
        }
        Err(_) => 0,
    };

    let volume = f32::from_bits(rt.shared.volume.load(Ordering::Relaxed));
    // The fade still runs — the gap is entered smoothly either way —
    // but a drain at end-of-track is expected and is not counted.
    if fill_output(out, got, rt.channels, target, volume, &mut rt.fade)
        && !rt.shared.draining.load(Ordering::Relaxed)
    {
        rt.shared.underruns.fetch_add(1, Ordering::Relaxed);
        // How short, not just that it happened: an occasional handful
        // of frames is a different problem from a whole period.
        rt.shared
            .last_short_frames
            .store(((want - got) / rt.channels) as u32, Ordering::Relaxed);
    }

    rt.shared
        .position_frames
        .fetch_add((got / rt.channels) as u64, Ordering::Relaxed);
}

/// Build the output stream.
pub(super) fn build_stream(
    device: &cpal::Device,
    config: StreamConfig,
    consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, AudioError> {
    let err_shared = Arc::clone(&shared);
    let mut rt = Rt::new(
        consumer,
        shared,
        config.channels as usize,
        config.sample_rate,
    );

    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], info: &cpal::OutputCallbackInfo| {
            // **The playback instant, not the callback instant** (I13, §11.5).
            // Verified against the pinned cpal 0.18.2: `OutputStreamTimestamp`
            // carries both, `playback` being "the predicted instant that data
            // written will be delivered to the device for playback".
            //
            // `as_nanos` is a `u128` in a timebase that starts at the stream,
            // so it cannot overflow a `u64` in any run this will see; the
            // saturating cast is there so that a backend reporting nonsense
            // yields a clamped instant rather than a wrapped one.
            let stamp = info.timestamp();
            let clock = RenderClock {
                callback_nanos: u64::try_from(stamp.callback.as_nanos()).unwrap_or(u64::MAX),
                playback_nanos: u64::try_from(stamp.playback.as_nanos()).unwrap_or(u64::MAX),
            };
            process(&mut rt, out, clock);
        },
        move |err| {
            // **Never print from here.** The TUI owns the alternate screen, so
            // anything written to stdout or stderr lands on top of the drawn
            // frame and corrupts it. Set a flag; the decoder turns it into a
            // `PlayerEvent` and the UI shows it in the status line.
            match err.kind() {
                // The OS rerouted us — headphones went in, or the default
                // output changed. cpal documents the stream as still active
                // and needing no rebuild, so this is news, not a failure.
                ErrorKind::DeviceChanged => {
                    err_shared.device_rerouted.store(true, Ordering::Release);
                }
                // Advisory: audio keeps playing, possibly with a glitch. Not
                // counted as a ring underrun, which measures something else.
                ErrorKind::Xrun | ErrorKind::RealtimeDenied | ErrorKind::DeviceBusy => {}
                // Anything else means the device is genuinely gone.
                _ => err_shared.device_lost.store(true, Ordering::Release),
            }
        },
        None,
    )?;

    Ok(stream)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// 8ms at 48k.
    const RAMP_FRAMES: usize = 384;

    /// A clock for the tests that are not about timing.
    ///
    /// Ten milliseconds of latency, which is the order a device reports, and a
    /// callback instant that does not advance — these tests assert on the ring,
    /// the fade and the allocator, none of which read it. The tests that *are*
    /// about the clock build their own.
    /// **A mapping names the first frame of the buffer, not the last** — #212.
    ///
    /// The instant a device reports is when *this* buffer reaches the output,
    /// so the frame it is paired with has to be the one at the buffer's head.
    /// Pairing it with the counter after rendering would name a frame that has
    /// not been handed over yet, and every position derived from it would be
    /// one buffer late — which is precisely the error I13 exists to remove,
    /// reintroduced from the other direction.
    #[test]
    fn a_mapping_names_the_first_frame_of_the_buffer() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        const PERIOD: usize = 256;

        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(crate::audio::Shared::new());
        let mut rt = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        let mut out = vec![0.0f32; PERIOD * CHANNELS];

        let audio = vec![0.25f32; PERIOD * CHANNELS * 4];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        process(&mut rt, &mut out, TEST_CLOCK);
        let first = shared.sounding().expect("a mapping after the first buffer");
        assert_eq!(first.frame, 0, "the first buffer starts at frame zero");

        process(&mut rt, &mut out, TEST_CLOCK);
        let second = shared.sounding().expect("a mapping after the second");
        assert_eq!(
            second.frame, PERIOD as u64,
            "the second buffer starts where the first ended"
        );
    }

    /// **A renderer publishes the stream it belongs to, not the one that is
    /// running** — #212.
    ///
    /// This is what makes a stale mapping detectable. A renderer that outlived
    /// its stream still writes to the same `Shared`; if it read the generation
    /// fresh each buffer it would adopt the new one and its stale frame count
    /// would be indistinguishable from the live stream's. Holding the
    /// generation it was built under is what lets a reader refuse it.
    #[test]
    fn a_renderer_publishes_its_own_generation_not_the_running_one() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        const PERIOD: usize = 64;

        let shared = Arc::new(crate::audio::Shared::new());
        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let mut stale = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        assert_eq!(stale.generation(), 1, "the first stream is generation one");

        // A second stream opens — the device came back, say — and the first
        // renderer has not stopped yet.
        let (_p2, consumer2) = crate::audio::ring::new(RATE, CHANNELS);
        let live = Rt::new(consumer2, Arc::clone(&shared), CHANNELS, RATE);
        assert_eq!(live.generation(), 2);
        assert_eq!(shared.generation(), 2, "the running stream is the second");

        let audio = vec![0.25f32; PERIOD * CHANNELS * 4];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let mut out = vec![0.0f32; PERIOD * CHANNELS];
        process(&mut stale, &mut out, TEST_CLOCK);

        let map = shared.sounding().expect("the stale renderer published");
        assert_eq!(
            map.generation, 1,
            "it published its own generation, so it can be told apart"
        );
        assert_ne!(
            map.generation,
            shared.generation(),
            "and a reader comparing against the running stream refuses it"
        );
    }

    /// **The published latency is the gap between the two instants** — I13.
    ///
    /// Not the callback instant, and not zero. This is the number a position
    /// is corrected by, so getting it from the wrong pair of instants puts the
    /// playhead exactly as wrong as having no correction at all.
    #[test]
    fn the_published_latency_is_the_gap_to_the_playback_instant() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        const PERIOD: usize = 128;

        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(crate::audio::Shared::new());
        let mut rt = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        let mut out = vec![0.0f32; PERIOD * CHANNELS];

        let audio = vec![0.25f32; PERIOD * CHANNELS * 4];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let clock = RenderClock {
            callback_nanos: 5_000_000,
            playback_nanos: 12_000_000,
        };
        process(&mut rt, &mut out, clock);

        let map = shared.sounding().expect("a mapping");
        assert_eq!(map.at_nanos, 12_000_000, "the instant is the playback one");
        assert_eq!(map.latency_nanos, 7_000_000, "and the latency is the gap");
    }

    /// A backend reporting a playback instant *before* its callback instant is
    /// talking nonsense. Saturating means that reads as no latency rather than
    /// as the near-six-hundred years a wrapped subtraction would produce — and
    /// a position corrected by that would be pinned at zero forever.
    #[test]
    fn a_playback_instant_before_the_callback_is_no_latency_not_an_enormous_one() {
        let backwards = RenderClock {
            callback_nanos: 9_000_000,
            playback_nanos: 1_000_000,
        };
        assert_eq!(backwards.latency_nanos(), 0);
    }

    /// **A half-written mapping is never handed out** — #212.
    ///
    /// The four values are one fact, and the seqlock is what makes reading
    /// them atomic. An odd sequence means the render step is mid-write, and
    /// the reader gives up rather than returning a frame paired with the
    /// previous buffer's instant.
    #[test]
    fn a_mapping_caught_mid_write_is_refused() {
        use std::sync::atomic::Ordering;

        let shared = crate::audio::Shared::new();
        shared.map_frame.store(1024, Ordering::Relaxed);
        shared.map_at_nanos.store(7, Ordering::Relaxed);
        // Odd: the writer is inside the pair of stores.
        shared.map_seq.store(3, Ordering::Relaxed);

        assert_eq!(
            shared.sounding(),
            None,
            "a torn read is refused rather than guessed at"
        );
    }

    /// Nothing has played yet, so there is nothing to report. Distinguished
    /// from a mapping that happens to be all zeroes, which is a real answer.
    #[test]
    fn no_stream_has_published_anything_yet() {
        let shared = crate::audio::Shared::new();
        assert_eq!(shared.sounding(), None);
        assert_eq!(shared.generation(), 0, "and no stream has opened");
    }

    /// **While audio flows, the instant and the frame advance together** —
    /// #212. That is what makes the pair a clock rather than two numbers.
    ///
    /// Driven directly rather than through a thread, because the relation only
    /// holds while the ring has audio: a renderer that runs past the end keeps
    /// publishing a running clock against a stalled frame count, which is
    /// correct and would make this assertion flaky if it were raced.
    #[test]
    fn a_published_instant_tracks_the_frames_while_audio_flows() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        const PERIOD: usize = 256;
        const BUFFERS: usize = 8;

        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(crate::audio::Shared::new());
        let mut rt = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        let mut out = vec![0.0f32; PERIOD * CHANNELS];

        let audio = vec![0.25f32; PERIOD * CHANNELS * (BUFFERS + 2)];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let per_buffer_nanos = (PERIOD as u64 * 1_000_000_000) / u64::from(RATE);
        let mut previous: Option<crate::audio::Sounding> = None;
        for buffer in 0..BUFFERS {
            let callback_nanos = buffer as u64 * per_buffer_nanos;
            process(
                &mut rt,
                &mut out,
                RenderClock {
                    callback_nanos,
                    playback_nanos: callback_nanos + per_buffer_nanos,
                },
            );
            let map = shared.sounding().expect("a mapping per buffer");
            assert_eq!(
                map.frame,
                buffer as u64 * PERIOD as u64,
                "buffer {buffer} starts where the previous one ended"
            );
            // **Asserted as a step, not as a formula.** `per_buffer_nanos` is
            // truncated, so `n` steps of it is not the same number as the time
            // at frame `n * PERIOD` computed in one division — off by a
            // nanosecond by the fourth buffer. The claim worth making is that
            // the frame and the instant move together, and that is exact.
            if let Some(last) = previous {
                assert_eq!(
                    map.frame - last.frame,
                    PERIOD as u64,
                    "one buffer of frames"
                );
                assert_eq!(
                    map.at_nanos - last.at_nanos,
                    per_buffer_nanos,
                    "and one buffer of time, in step with it"
                );
            }
            previous = Some(map);
        }
    }

    const TEST_CLOCK: RenderClock = RenderClock {
        callback_nanos: 0,
        playback_nanos: 10_000_000,
    };

    /// **The render step allocates nothing, on any path it can take** — I2.
    ///
    /// Not a review and not a comment: a global allocator counts, and this
    /// drives every branch `process` has — the seek handshake, silence, a fed
    /// ring, and a starved one. The starved path is the one worth having,
    /// because it is the branch that runs when something has already gone
    /// wrong and is the least likely to be exercised by hand.
    #[test]
    fn the_render_step_allocates_nothing() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        const PERIOD: usize = 256;

        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(crate::audio::Shared::new());
        let mut rt = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        // Allocated here, so the guarded region below does not have to.
        let mut out = vec![0.0f32; PERIOD * CHANNELS];

        // Fill the ring outside the guard; the decoder is not the RT side.
        let audio = vec![0.25f32; PERIOD * CHANNELS * 4];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }

        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let (_, violations) = crate::rt_guard::forbidden(|| {
            // A fed ring.
            process(&mut rt, &mut out, TEST_CLOCK);
            // A seek: the flush handshake discards and repositions.
            shared.flush_seq.fetch_add(1, Ordering::Release);
            process(&mut rt, &mut out, TEST_CLOCK);
            // Starved: the ring is empty now, so this takes the fade-to-silence
            // path and bumps the underrun counter.
            process(&mut rt, &mut out, TEST_CLOCK);
            process(&mut rt, &mut out, TEST_CLOCK);
            // Paused, then fully silent: the early return.
            shared.state.store(State::Paused as u8, Ordering::Relaxed);
            for _ in 0..64 {
                process(&mut rt, &mut out, TEST_CLOCK);
            }
        });

        assert_eq!(
            violations, 0,
            "the render step allocated, which is a click waiting to happen"
        );
    }

    /// **And none of those paths panics.** A panic crossing the cpal FFI
    /// boundary is undefined behaviour, which is why the rule lists it — and
    /// why it is worth driving the branches rather than trusting that an
    /// index is in range.
    #[test]
    fn the_render_step_survives_an_empty_ring_and_a_seek() {
        use crate::audio::State;
        use std::sync::atomic::Ordering;

        const CHANNELS: usize = 2;
        let (_producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(crate::audio::Shared::new());
        let mut rt = Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);

        // Buffer sizes a device might actually pick, including one frame.
        for period in [1usize, 64, 256, 1024] {
            let mut out = vec![0.0f32; period * CHANNELS];
            shared.state.store(State::Playing as u8, Ordering::Relaxed);
            process(&mut rt, &mut out, TEST_CLOCK);
            shared.flush_seq.fetch_add(1, Ordering::Release);
            process(&mut rt, &mut out, TEST_CLOCK);
            shared.state.store(State::Stopped as u8, Ordering::Relaxed);
            process(&mut rt, &mut out, TEST_CLOCK);
        }

        assert!(
            shared.underruns.load(Ordering::Relaxed) > 0,
            "an empty ring should have been reported, not silently tolerated"
        );
    }

    fn ones(frames: usize, channels: usize) -> Vec<f32> {
        vec![1.0; frames * channels]
    }

    /// The property that matters: no sample-to-sample jump big enough to hear
    /// as a click. A hard cut to silence would show a step of 1.0 here.
    fn largest_step(buf: &[f32], channels: usize) -> f32 {
        buf.chunks(channels)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max)
    }

    #[test]
    fn fade_in_reaches_full_level_within_the_ramp() {
        let mut fade = Fade::new(RATE);
        let mut buf = ones(RAMP_FRAMES + 64, 2);
        fade.apply(&mut buf, 2, 1.0, 1.0);
        let last = *buf.last().unwrap();
        assert!(
            (last - 1.0).abs() < 1e-3,
            "ended at {last}, expected full level"
        );
        assert!(
            buf[0].abs() < 0.05,
            "should start near silence, got {}",
            buf[0]
        );
    }

    #[test]
    fn fade_out_reaches_silence_and_then_stays_there() {
        let mut fade = Fade::new(RATE);
        // Get to full level first.
        fade.apply(&mut ones(RAMP_FRAMES, 2), 2, 1.0, 1.0);

        let mut buf = ones(RAMP_FRAMES + 64, 2);
        fade.apply(&mut buf, 2, 0.0, 1.0);
        assert!(buf.last().unwrap().abs() < 1e-3, "did not reach silence");
        assert!(fade.is_silent(0.0), "should report silence once faded out");
    }

    /// The whole point: a transport change must not produce a step.
    #[test]
    fn ramping_never_steps_more_than_a_hair() {
        for target in [0.0f32, 1.0] {
            let mut fade = Fade::new(RATE);
            if target == 0.0 {
                fade.apply(&mut ones(RAMP_FRAMES, 2), 2, 1.0, 1.0);
            }
            let mut buf = ones(RAMP_FRAMES * 2, 2);
            fade.apply(&mut buf, 2, target, 1.0);
            let step = largest_step(&buf, 2);
            assert!(
                step < 0.02,
                "target {target}: largest jump {step}, that is audible"
            );
        }
    }

    /// Rapid pause/resume — the case that surfaced this — must stay smooth
    /// even when the direction reverses mid-ramp.
    #[test]
    fn reversing_mid_ramp_is_still_smooth() {
        let mut fade = Fade::new(RATE);
        fade.apply(&mut ones(RAMP_FRAMES, 2), 2, 1.0, 1.0);

        let mut all = Vec::new();
        // Fade out only part way, then reverse, repeatedly.
        for target in [0.0f32, 1.0, 0.0, 1.0] {
            let mut buf = ones(RAMP_FRAMES / 3, 2);
            fade.apply(&mut buf, 2, target, 1.0);
            all.extend_from_slice(&buf);
        }
        let step = largest_step(&all, 2);
        assert!(step < 0.02, "reversal produced a jump of {step}");
    }

    #[test]
    fn volume_scales_without_disturbing_the_ramp() {
        let mut fade = Fade::new(RATE);
        let mut buf = ones(RAMP_FRAMES * 2, 2);
        fade.apply(&mut buf, 2, 1.0, 0.5);
        let last = *buf.last().unwrap();
        assert!((last - 0.5).abs() < 1e-3, "volume not applied: {last}");
        assert!(largest_step(&buf, 2) < 0.02);
    }

    /// A seek cuts to silence deliberately: the audio either side is
    /// unrelated, so it must fade in rather than splice.
    #[test]
    fn cut_forces_a_fade_in_after_a_seek() {
        let mut fade = Fade::new(RATE);
        fade.apply(&mut ones(RAMP_FRAMES, 2), 2, 1.0, 1.0);
        fade.cut();
        assert!(fade.is_silent(0.0));

        let mut buf = ones(RAMP_FRAMES * 2, 2);
        fade.apply(&mut buf, 2, 1.0, 1.0);
        assert!(buf[0].abs() < 0.05, "should start from silence");
        assert!(largest_step(&buf, 2) < 0.02);
    }

    #[test]
    fn mono_and_multichannel_ramp_together() {
        for channels in [1usize, 2, 6] {
            let mut fade = Fade::new(RATE);
            let mut buf = ones(RAMP_FRAMES, channels);
            fade.apply(&mut buf, channels, 1.0, 1.0);
            // Every channel in a frame gets the same gain, so the last frame
            // must be uniform.
            let last: Vec<f32> = buf.chunks(channels).next_back().unwrap().to_vec();
            for v in &last {
                assert!((v - last[0]).abs() < 1e-6, "channels drifted: {last:?}");
            }
        }
    }
    // ---- the callback's own fill path ------------------------------------
    //
    // These drive `fill_output`, not `Fade`, because the discontinuity that
    // prompted them lived in the buffer handling *around* the ramp and was
    // invisible to a test that only drove the ramp.

    /// A full-scale signal, so any step shows at its worst.
    fn signal(frames: usize, channels: usize, phase: &mut f32) -> Vec<f32> {
        let mut out = Vec::with_capacity(frames * channels);
        for _ in 0..frames {
            let v = (*phase).sin() * 0.8;
            *phase += 0.05;
            for _ in 0..channels {
                out.push(v);
            }
        }
        out
    }

    /// Feed `fill_output` a sequence of (available frames, playing) periods and
    /// return every sample of channel 0 that came out.
    fn run_periods(periods: &[(usize, bool)], period_frames: usize, channels: usize) -> Vec<f32> {
        let mut fade = Fade::new(RATE);
        let mut phase = 0.0f32;
        let mut produced = Vec::new();
        for &(got, playing) in periods {
            let mut buf = signal(period_frames, channels, &mut phase);
            let target = if playing { 1.0 } else { 0.0 };
            fill_output(&mut buf, got * channels, channels, target, 1.0, &mut fade);
            produced.extend(buf.chunks(channels).map(|f| f[0]));
        }
        produced
    }

    /// The bug this file was opened for: a ring that runs dry mid-playback used
    /// to cut the last real sample straight to zero, and jump back to full
    /// level when it recovered. Two clicks per underrun.
    #[test]
    fn ring_starvation_ramps_instead_of_cutting() {
        let period = 256;
        let out = run_periods(
            &[
                (period, true), // playing normally
                (period, true),
                (64, true),     // ring runs dry part way through
                (0, true),      // fully dry
                (period, true), // recovered
                (period, true),
            ],
            period,
            2,
        );
        let step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(
            step < 0.05,
            "starvation produced a jump of {step}; that is the click"
        );
    }

    #[test]
    fn recovery_after_starvation_fades_back_in() {
        let period = 256;
        let out = run_periods(&[(period, true), (0, true), (period, true)], period, 2);
        // The first sample of the recovered period must start from silence,
        // not resume at full level.
        let recovered = &out[period * 2..];
        assert!(
            recovered[0].abs() < 0.05,
            "recovery jumped straight back to {}",
            recovered[0]
        );
    }

    #[test]
    fn a_full_period_is_untouched_apart_from_the_ramp() {
        let period = 512;
        let out = run_periods(&[(period, true), (period, true), (period, true)], period, 2);
        // By the third period the ramp is long finished, so amplitude should be
        // the signal's own 0.8 peak rather than anything attenuated.
        let peak = out[period * 2..].iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(
            peak > 0.7,
            "steady-state playback is attenuated: peak {peak}"
        );
    }

    #[test]
    fn pause_and_resume_across_periods_stays_smooth() {
        let period = 256;
        let out = run_periods(
            &[
                (period, true),
                (period, true),
                (period, false), // pause
                (period, false),
                (period, true), // resume
                (period, true),
            ],
            period,
            2,
        );
        let step = out
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.05, "pause/resume produced a jump of {step}");
    }

    #[test]
    fn starvation_is_reported() {
        let mut fade = Fade::new(RATE);
        let mut buf = vec![0.5f32; 512];
        assert!(fill_output(&mut buf, 100, 2, 1.0, 1.0, &mut fade));

        let mut buf = vec![0.5f32; 512];
        assert!(!fill_output(&mut buf, 512, 2, 1.0, 1.0, &mut fade));
    }
    /// A drain at end-of-track is not starvation, and counting it turns the
    /// underrun figure into a count of tracks played.
    ///
    /// This regressed once already: the guard existed in M1 and was lost when
    /// the callback moved into this file in M2. A 45-minute listening test
    /// then logged exactly one underrun per track, every one at the track's
    /// own duration.
    #[test]
    fn a_drain_while_draining_is_not_an_underrun() {
        use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

        // The decision the callback makes, in isolation: starved *and* not
        // draining is the only case that counts.
        let draining = AtomicBool::new(false);
        let underruns = AtomicU32::new(0);

        let count_if = |starved: bool| {
            if starved && !draining.load(Ordering::Relaxed) {
                underruns.fetch_add(1, Ordering::Relaxed);
            }
        };

        // Mid-track starvation still counts — that is the thing worth knowing.
        count_if(true);
        assert_eq!(underruns.load(Ordering::Relaxed), 1);

        // The same starvation at end-of-track does not.
        draining.store(true, Ordering::Relaxed);
        for _ in 0..20 {
            count_if(true);
        }
        assert_eq!(
            underruns.load(Ordering::Relaxed),
            1,
            "the end-of-track drain was counted"
        );

        // And the next track counts again.
        draining.store(false, Ordering::Relaxed);
        count_if(true);
        assert_eq!(underruns.load(Ordering::Relaxed), 2);
    }

    /// The gap is still entered smoothly: not counting it must not mean not
    /// fading it, or every track would end in a click.
    #[test]
    fn a_drain_still_fades_even_when_not_counted() {
        let mut fade = Fade::new(RATE);
        fade.apply(&mut ones(RAMP_FRAMES, 2), 2, 1.0, 1.0);

        let mut buf = ones(256, 2);
        let starved = fill_output(&mut buf, 64 * 2, 2, 1.0, 1.0, &mut fade);
        assert!(starved, "should still report the shortfall to its caller");

        let step = buf
            .chunks(2)
            .map(|f| f[0])
            .collect::<Vec<_>>()
            .windows(2)
            .map(|w| (w[1] - w[0]).abs())
            .fold(0.0f32, f32::max);
        assert!(step < 0.05, "the gap was entered with a jump of {step}");
    }
}
