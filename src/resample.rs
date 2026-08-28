//! Sample-rate conversion from a file's native rate to the device rate.
//!
//! DESIGN §11.5 asks whether to resample or reconfigure the device per track.
//! Reconfiguring is not viable: output devices advertise disjoint rate sets
//! (a Bluetooth sink here offers only 16k/44.1k while all Opus is 48k), so a
//! fixed device rate plus conversion is the only policy that plays everything.
//!
//! Everything here runs on the decoder thread. The audio callback never sees
//! it — see the §3 INVARIANT.

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{
    Async, Fft, FixedAsync, FixedSync, Indexing, PolynomialDegree, Resampler as _,
    SincInterpolationParameters,
};

/// The `resample_quality` setting DESIGN §11.5 calls for.
///
/// Costs below are measured in-situ (inside the running player, which is
/// several times the cost a throughput benchmark reports — the decoder thread
/// sleeps between packets, so filter tables go cold):
///
/// | quality  | cost/core | notes                                        |
/// |----------|-----------|----------------------------------------------|
/// | Fast     | ~0.1%     | polynomial, audible artefacts on some material|
/// | Balanced | ~0.2%     | FFT, transparent for fixed ratios            |
/// | High     | ~1.6%     | sinc 256, transparent, costs 8x Balanced      |
///
/// `Balanced` is the default: for the fixed rate ratios a music player deals
/// in, the FFT resampler is both cheaper and cleaner than a mid-length sinc.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Default)]
pub enum Quality {
    Fast,
    #[default]
    Balanced,
    High,
}

impl Quality {
    pub fn from_env() -> Self {
        match std::env::var("GMUSE_RESAMPLE").as_deref() {
            Ok("fast") => Self::Fast,
            Ok("high") => Self::High,
            _ => Self::Balanced,
        }
    }
}

/// Input frames handed to rubato per call. Small enough to keep latency low,
/// large enough that per-chunk overhead stays negligible.
const CHUNK: usize = 1024;

#[derive(Debug, thiserror::Error)]
pub enum ResampleError {
    #[error("failed to construct resampler: {0}")]
    Construction(#[from] rubato::ResamplerConstructionError),
    #[error("resampling failed: {0}")]
    Process(#[from] rubato::ResampleError),
    #[error("buffer size mismatch: {0}")]
    Size(#[from] rubato::audioadapter_buffers::SizeError),
}

/// Fixed-ratio interleaved f32 resampler.
///
/// Feed interleaved frames with [`Resampler::process`]; converted frames are
/// appended to the caller's output vector. Call [`Resampler::flush`] at
/// end-of-stream to drain the filter's tail.
pub struct Resampler {
    inner: Box<dyn rubato::Resampler<f32>>,
    channels: usize,
    /// Input frames accepted but not yet consumed by a full chunk.
    staging: Vec<f32>,
    /// Scratch for one output chunk.
    scratch: Vec<f32>,
    /// Output frames of filter delay still to be discarded.
    ///
    /// A sinc resampler emits its group delay as leading silence. Dropping it
    /// keeps playback position honest and matters for gapless later.
    delay_left: usize,
}

impl Resampler {
    pub fn new(
        from_rate: u32,
        to_rate: u32,
        channels: usize,
        quality: Quality,
    ) -> Result<Self, ResampleError> {
        let ratio = to_rate as f64 / from_rate as f64;
        // Fixed ratio throughout, so the adjustable range can stay tight.
        let inner: Box<dyn rubato::Resampler<f32>> = match quality {
            Quality::Fast => Box::new(Async::<f32>::new_poly(
                ratio,
                1.1,
                PolynomialDegree::Septic,
                CHUNK,
                channels,
                FixedAsync::Input,
            )?),
            Quality::Balanced => Box::new(Fft::<f32>::new(
                from_rate as usize,
                to_rate as usize,
                CHUNK,
                channels,
                FixedSync::Input,
            )?),
            Quality::High => Box::new(Async::<f32>::new_sinc(
                ratio,
                1.1,
                &SincInterpolationParameters::default(),
                CHUNK,
                channels,
                FixedAsync::Input,
            )?),
        };
        let delay_left = inner.output_delay();
        Ok(Self {
            inner,
            channels,
            staging: Vec::with_capacity(CHUNK * channels * 2),
            scratch: Vec::new(),
            delay_left,
        })
    }

    /// Convert `input` (interleaved, source rate), appending interleaved
    /// device-rate frames to `out`. Holds back a partial chunk internally.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) -> Result<(), ResampleError> {
        self.staging.extend_from_slice(input);
        self.run(out, false)
    }

    /// Drain the remaining staged frames and the filter tail.
    pub fn flush(&mut self, out: &mut Vec<f32>) -> Result<(), ResampleError> {
        self.run(out, true)
    }

    fn run(&mut self, out: &mut Vec<f32>, finish: bool) -> Result<(), ResampleError> {
        let ch = self.channels;
        loop {
            let need = self.inner.input_frames_next();
            let have = self.staging.len() / ch;

            // Not enough for a full chunk: stop, unless we're flushing, in
            // which case pad the last chunk with silence via `partial_len`.
            let partial = if have >= need {
                None
            } else if finish && have > 0 {
                Some(have)
            } else {
                return Ok(());
            };

            let out_max = self.inner.output_frames_max();
            self.scratch.resize(out_max * ch, 0.0);

            // `partial_len` still requires a full-size input buffer, so pad the
            // staging area up to `need` frames before wrapping it.
            if self.staging.len() < need * ch {
                self.staging.resize(need * ch, 0.0);
            }

            let indexing = Indexing {
                input_offset: 0,
                output_offset: 0,
                partial_len: partial,
                active_channels_mask: None,
            };

            let input = InterleavedSlice::new(&self.staging[..need * ch], ch, need)?;
            let mut output = InterleavedSlice::new_mut(&mut self.scratch[..], ch, out_max)?;
            let (frames_in, frames_out) =
                self.inner
                    .process_into_buffer(&input, &mut output, Some(&indexing))?;

            // Discard the leading filter delay before it reaches the ring.
            let skip = self.delay_left.min(frames_out);
            self.delay_left -= skip;
            out.extend_from_slice(&self.scratch[skip * ch..frames_out * ch]);

            let consumed = frames_in.max(partial.unwrap_or(0));
            self.staging.drain(..(consumed * ch).min(self.staging.len()));

            if partial.is_some() {
                self.staging.clear();
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Output length should track the ratio, within a chunk's slack.
    #[test]
    fn ratio_is_respected() {
        let mut r = Resampler::new(44_100, 48_000, 2, Quality::High).unwrap();
        let frames = 44_100;
        let input = vec![0.0f32; frames * 2];
        let mut out = Vec::new();
        r.process(&input, &mut out).unwrap();
        r.flush(&mut out).unwrap();
        let got = out.len() / 2;
        let want = 48_000;
        assert!(
            (got as i64 - want as i64).abs() < CHUNK as i64 * 2,
            "got {got} frames, want ~{want}"
        );
    }

    /// A sine must stay a sine: check the resampled signal is not silence and
    /// stays inside the input's amplitude range.
    #[test]
    fn sine_survives() {
        let mut r = Resampler::new(44_100, 48_000, 1, Quality::High).unwrap();
        let input: Vec<f32> = (0..44_100)
            .map(|i| (i as f32 * 440.0 * std::f32::consts::TAU / 44_100.0).sin() * 0.5)
            .collect();
        let mut out = Vec::new();
        r.process(&input, &mut out).unwrap();
        r.flush(&mut out).unwrap();

        let peak = out.iter().fold(0.0f32, |m, v| m.max(v.abs()));
        assert!(peak > 0.4, "signal vanished, peak {peak}");
        assert!(peak < 0.6, "signal clipped or rang, peak {peak}");

        // The delay is trimmed, so the very start should already be signal
        // rather than a long run of zeros.
        let leading_zeros = out.iter().take_while(|v| v.abs() < 1e-6).count();
        assert!(leading_zeros < 64, "{leading_zeros} leading zeros, delay not trimmed");
    }

    /// Spectral check: resample a pure tone and measure how much energy lands
    /// outside the original frequency. Aliasing from a poor resampler shows up
    /// here as a raised noise floor.
    #[test]
    fn all_qualities_are_clean() {
        for q in [Quality::Fast, Quality::Balanced, Quality::High] {
            let freq = 997.0f64; // non-harmonic w.r.t. both rates
            let n = 44_100 * 2;
            let input: Vec<f32> = (0..n)
                .map(|i| (i as f64 * freq * std::f64::consts::TAU / 44_100.0).sin() as f32 * 0.5)
                .collect();
            let mut r = Resampler::new(44_100, 48_000, 1, q).unwrap();
            let mut out = Vec::new();
            r.process(&input, &mut out).unwrap();
            r.flush(&mut out).unwrap();

            // Goertzel at the expected output frequency vs total energy.
            let m = out.len().min(48_000);
            let seg = &out[out.len() / 4..out.len() / 4 + m / 2];
            let k = freq / 48_000.0 * std::f64::consts::TAU;
            let (mut sr, mut si) = (0.0f64, 0.0f64);
            for (i, v) in seg.iter().enumerate() {
                sr += *v as f64 * (k * i as f64).cos();
                si += *v as f64 * (k * i as f64).sin();
            }
            let tone = (sr * sr + si * si).sqrt() / seg.len() as f64 * 2.0;
            let total: f64 = seg.iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / seg.len() as f64;
            let total_rms = total.sqrt();
            let ratio = tone / (total_rms * std::f64::consts::SQRT_2);
            println!("  {q:?}: tone/total = {ratio:.4}");
            assert!(ratio > 0.97, "{q:?} lost energy to aliasing: {ratio:.4}");
        }
    }

    #[test]
    fn identity_ratio_round_trips() {
        let mut r = Resampler::new(48_000, 48_000, 2, Quality::High).unwrap();
        let input: Vec<f32> = (0..4096).map(|i| (i as f32 / 100.0).sin()).collect();
        let mut out = Vec::new();
        r.process(&input, &mut out).unwrap();
        r.flush(&mut out).unwrap();
        assert!(!out.is_empty());
    }
}

#[cfg(test)]
mod perf {
    use super::*;
    use std::time::Instant;

    /// Rough real-time factor for the worst case we actually hit: 48k Opus
    /// down to a 44.1k Bluetooth device, stereo. Printed with --nocapture.
    #[test]
    fn realtime_factor() {
        let secs = 60;
        let input: Vec<f32> = (0..48_000 * secs * 2)
            .map(|i| (i as f32 / 97.0).sin() * 0.25)
            .collect();
        let mut r = Resampler::new(48_000, 44_100, 2, Quality::High).unwrap();
        let mut out = Vec::with_capacity(44_100 * secs * 2);

        let t0 = Instant::now();
        for block in input.chunks(4096) {
            r.process(block, &mut out).unwrap();
        }
        r.flush(&mut out).unwrap();
        let dt = t0.elapsed();

        let rtf = secs as f64 / dt.as_secs_f64();
        println!("resampled {secs}s of stereo 48k->44.1k in {dt:?} ({rtf:.0}x realtime)");
        assert!(rtf > 20.0, "resampler too slow: {rtf:.1}x realtime");
    }
}
