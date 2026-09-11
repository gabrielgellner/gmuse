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
pub struct Rt {
    consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
    channels: usize,
    /// The last flush the render step has serviced. Not an atomic: only this
    /// step reads or writes it.
    serviced: u64,
    fade: Fade,
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
        Rt {
            consumer,
            channels,
            fade: Fade::new(rate),
            shared,
            serviced: 0,
        }
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
pub fn process(rt: &mut Rt, out: &mut [f32]) {
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
        return;
    }

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
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| process(&mut rt, out),
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
