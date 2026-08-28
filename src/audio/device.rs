//! Output device selection and the real-time callback.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use cpal::traits::{DeviceTrait, HostTrait};
use cpal::{SampleFormat, StreamConfig, SupportedBufferSize};

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

/// Build the output stream.
///
/// **INVARIANT (DESIGN §3).** Inside the callback: no allocation, no locking,
/// no syscalls, no panics, no unbounded loops. It reads the ring, applies gain,
/// writes the output slice, and updates atomics. Nothing else, ever.
pub(super) fn build_stream(
    device: &cpal::Device,
    config: StreamConfig,
    mut consumer: rtrb::Consumer<f32>,
    shared: Arc<Shared>,
) -> Result<cpal::Stream, AudioError> {
    let channels = config.channels as usize;
    let err_shared = Arc::clone(&shared);

    // The last flush the callback has serviced. Callback-local, so no atomic.
    let mut serviced: u64 = 0;

    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Seek/stop handshake: discard buffered audio and adopt the new
            // position. `read_chunk(..).commit_all()` moves an index; it does
            // not copy or free, so this is safe in the RT context.
            let requested = shared.flush_seq.load(Ordering::Acquire);
            if requested != serviced {
                let stale = consumer.slots();
                if stale > 0
                    && let Ok(chunk) = consumer.read_chunk(stale)
                {
                    chunk.commit_all();
                }
                shared.position_frames.store(
                    shared.flush_position.load(Ordering::Relaxed),
                    Ordering::Relaxed,
                );
                serviced = requested;
                shared.flush_ack.store(requested, Ordering::Release);
                out.fill(0.0);
                return;
            }

            // Paused and stopped both hold the device open and emit silence.
            // Stopping the stream instead would risk a click on resume, and
            // would drop the ring contents we want to resume from.
            if State::from_u8(shared.state.load(Ordering::Relaxed)) != State::Playing {
                out.fill(0.0);
                return;
            }

            let want = out.len();
            let got = match consumer.read_chunk(want.min(consumer.slots())) {
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

            if got < want {
                out[got..].fill(0.0);
                shared.underruns.fetch_add(1, Ordering::Relaxed);
            }

            let gain = f32::from_bits(shared.volume.load(Ordering::Relaxed));
            if gain != 1.0 {
                for s in out[..got].iter_mut() {
                    *s *= gain;
                }
            }

            shared
                .position_frames
                .fetch_add((got / channels) as u64, Ordering::Relaxed);
        },
        move |err| {
            // Also forbidden to allocate here in principle, but this fires on
            // device loss, not per period, and the decoder turns the flag into
            // a real event.
            err_shared.device_lost.store(true, Ordering::Release);
            eprintln!("stream error: {err}");
        },
        None,
    )?;

    Ok(stream)
}
