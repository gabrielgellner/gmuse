//! M1 — sound out of the speakers.
//!
//! Decode a file with Symphonia, push interleaved f32 frames through an rtrb
//! ring buffer, drain it from a cpal callback. No UI, no library. The device
//! is opened at its preferred rate and the decoder resamples to match.
//!
//! Verification target from DESIGN §9: plays a full album with no clicks or
//! dropouts and an underrun count of zero.

mod resample;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig, SupportedBufferSize};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

/// Shared state written by the audio callback, read by everyone else.
/// The callback may touch *only* these — see DESIGN §3 INVARIANT.
struct Shared {
    frames_played: AtomicU64,
    underruns: AtomicU32,
    /// Decoder has pushed its last frame; once the ring drains, we're done.
    decode_done: AtomicBool,
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args().nth(1).ok_or(
        "usage: gmuse <audio-file>\n\
         (M1: plays one file to completion and reports underruns)",
    )?;

    // ---- decoder setup -------------------------------------------------
    let file = std::fs::File::open(&path)?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = std::path::Path::new(&path)
        .extension()
        .and_then(|e| e.to_str())
    {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe().probe(
        &hint,
        mss,
        FormatOptions::default(),
        MetadataOptions::default(),
    )?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or("no audio track in file")?;
    let track_id = track.id;
    let audio_params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or("track has no audio codec parameters")?
        .clone();

    let codecs = codec_registry();
    let mut decoder = codecs.make_audio_decoder(&audio_params, &AudioDecoderOptions::default())?;

    // Decode the first packet up front. The codec parameters can lack a sample
    // rate (some MP3s), but a decoded buffer always knows its own spec, so this
    // is the honest source of truth for how to open the device.
    let (rate, channels, first_chunk) = {
        loop {
            let Some(packet) = format.next_packet()? else {
                return Err("file contains no decodable audio".into());
            };
            if packet.track_id != track_id {
                continue;
            }
            match decoder.decode(&packet) {
                Ok(buf) => {
                    let (rate, chans) = {
                        let spec = buf.spec();
                        (spec.rate(), spec.channels().count())
                    };
                    let mut interleaved = vec![0f32; buf.samples_interleaved()];
                    buf.copy_to_slice_interleaved(&mut interleaved[..]);
                    break (rate, chans, interleaved);
                }
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(e.into()),
            }
        }
    };

    println!("{path}");
    println!("  {rate} Hz, {channels} ch");

    // ---- device setup --------------------------------------------------
    let host = cpal::default_host();
    // GMUSE_DEVICE=<substring> selects an output device by id; default device
    // otherwise. Temporary M1 affordance — real device selection is a setting.
    let device = match std::env::var("GMUSE_DEVICE") {
        Ok(want) => host
            .output_devices()?
            .find(|d| {
                d.id()
                    .map(|i| i.to_string().contains(&want))
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("no output device matching {want:?}"))?,
        Err(_) => host
            .default_output_device()
            .ok_or("no default output device")?,
    };

    let config = pick_config(&device, rate, channels)?;
    println!(
        "  device: {} @ {} Hz, {} ch",
        device.id().map(|i| i.to_string()).unwrap_or_default(),
        config.sample_rate,
        config.channels
    );

    // ---- ring buffer ---------------------------------------------------
    // ~1000 ms of audio at the device rate (DESIGN §3: 500-1000ms).
    let capacity = (config.sample_rate as usize) * channels;
    let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(capacity);

    // Convert only when the device cannot take the file's rate directly.
    let device_rate = config.sample_rate;
    let resampler = if device_rate == rate {
        println!("  no resampling needed");
        None
    } else {
        let q = resample::Quality::from_env();
        println!("  resampling {rate} -> {device_rate} Hz ({q:?})");
        Some(resample::Resampler::new(rate, device_rate, channels, q)?)
    };

    let shared = Arc::new(Shared {
        frames_played: AtomicU64::new(0),
        underruns: AtomicU32::new(0),
        decode_done: AtomicBool::new(false),
    });

    // ---- audio callback ------------------------------------------------
    // INVARIANT (DESIGN §3): no allocation, no locks, no syscalls, no panics.
    // Only ring reads and atomic stores.
    let cb_shared = Arc::clone(&shared);
    let channels_usize = channels;
    let stream = device.build_output_stream(
        config,
        move |out: &mut [f32], _: &cpal::OutputCallbackInfo| {
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
                // Underrun: fill the tail with silence rather than block.
                out[got..].fill(0.0);
                // Only count it as an underrun if the decoder still owes us
                // audio — a short final callback at end-of-stream is normal.
                if !cb_shared.decode_done.load(Ordering::Relaxed) {
                    cb_shared.underruns.fetch_add(1, Ordering::Relaxed);
                }
            }

            cb_shared
                .frames_played
                .fetch_add((got / channels_usize) as u64, Ordering::Relaxed);
        },
        |err| eprintln!("stream error: {err}"),
        None,
    )?;

    // ---- decoder thread ------------------------------------------------
    let dec_shared = Arc::clone(&shared);
    let decode_thread = std::thread::spawn(move || -> Result<(), String> {
        let mut resampler = resampler;
        let mut scratch: Vec<f32> = Vec::new(); // decoded, source rate
        let mut staged: Vec<f32> = Vec::new(); // device rate, ready to push
        let mut first = Some(first_chunk);
        let mut resample_time = Duration::ZERO;
        let mut resample_calls = 0u64;
        let mut resample_frames = 0usize;

        loop {
            // Fetch the next decoded chunk at the source rate.
            let got_chunk = if let Some(f) = first.take() {
                scratch.clear();
                scratch.extend_from_slice(&f);
                true
            } else {
                loop {
                    match format.next_packet() {
                        Ok(Some(p)) if p.track_id != track_id => continue,
                        Ok(Some(p)) => match decoder.decode(&p) {
                            Ok(buf) => {
                                scratch.resize(buf.samples_interleaved(), 0.0);
                                buf.copy_to_slice_interleaved(&mut scratch[..]);
                                break true;
                            }
                            // Skip corrupt packets rather than abandoning the track.
                            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
                            Err(e) => return Err(format!("decode error: {e}")),
                        },
                        Ok(None) => break false,
                        Err(e) => return Err(format!("demux error: {e}")),
                    }
                }
            };

            staged.clear();
            if got_chunk {
                match resampler.as_mut() {
                    Some(r) => {
                        let t0 = std::time::Instant::now();
                        r.process(&scratch, &mut staged)
                            .map_err(|e| e.to_string())?;
                        resample_time += t0.elapsed();
                        resample_calls += 1;
                        resample_frames += scratch.len() / channels;
                    }
                    None => staged.extend_from_slice(&scratch),
                }
                push_all(&mut producer, &staged);
            } else {
                // End of stream: drain the resampler's tail before finishing.
                if let Some(r) = resampler.as_mut() {
                    r.flush(&mut staged).map_err(|e| e.to_string())?;
                    push_all(&mut producer, &staged);
                }
                break;
            }
        }

        if resample_calls > 0 && std::env::var("GMUSE_STATS").is_ok() {
            eprintln!(
                "\n  [resample] {resample_calls} calls, {resample_frames} frames, \
                 {:.3}s cpu, mean block {} frames",
                resample_time.as_secs_f64(),
                resample_frames / resample_calls as usize
            );
        }
        dec_shared.decode_done.store(true, Ordering::Release);
        Ok(())
    });

    // ---- run -----------------------------------------------------------
    stream.play()?;

    loop {
        std::thread::sleep(Duration::from_millis(200));
        let frames = shared.frames_played.load(Ordering::Relaxed);
        let secs = frames as f64 / device_rate as f64;
        print!(
            "\r  {:02}:{:05.2}  underruns: {}   ",
            (secs as u64) / 60,
            secs % 60.0,
            shared.underruns.load(Ordering::Relaxed)
        );
        use std::io::Write;
        let _ = std::io::stdout().flush();

        if shared.decode_done.load(Ordering::Acquire) && decode_thread.is_finished() {
            // Let the ring drain before tearing the stream down, otherwise we
            // truncate the tail of the track.
            std::thread::sleep(Duration::from_millis(1200));
            break;
        }
    }
    println!();

    drop(stream);
    match decode_thread.join() {
        Ok(Ok(())) => {}
        Ok(Err(e)) => return Err(e.into()),
        Err(_) => return Err("decoder thread panicked".into()),
    }

    let underruns = shared.underruns.load(Ordering::Relaxed);
    let played = shared.frames_played.load(Ordering::Relaxed) as f64 / device_rate as f64;
    println!("done. played {played:.3}s, underruns: {underruns}");
    if underruns > 0 {
        eprintln!("WARNING: M1 acceptance requires zero underruns");
    }
    Ok(())
}

/// Symphonia's enabled decoders, plus libopus when the `opus` feature is on.
///
/// Symphonia has no native Opus decoder, so `.opus`/Ogg-Opus needs the adapter
/// registered into a custom registry rather than `default::get_codecs()`.
fn codec_registry() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    #[cfg(feature = "opus")]
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    registry
}

/// Choose an output config, preferring the file's own rate so that no
/// conversion is needed; otherwise the nearest rate the device offers.
///
/// DESIGN §11.5: always resample rather than reconfigure the device per track.
/// Devices advertise disjoint rate sets, so a rate the file needs may simply
/// not exist on the current output.
fn pick_config(
    device: &cpal::Device,
    rate: u32,
    channels: usize,
) -> Result<StreamConfig, Box<dyn std::error::Error>> {
    let candidates: Vec<_> = device
        .supported_output_configs()?
        .filter(|c| c.sample_format() == SampleFormat::F32 && c.channels() as usize == channels)
        .collect();

    if candidates.is_empty() {
        return Err(format!("device has no f32 / {channels} ch output config").into());
    }

    // Every rate this device can actually be opened at, clamped into range.
    let mut options: Vec<(u32, &cpal::SupportedStreamConfigRange)> = candidates
        .iter()
        .map(|c| (rate.clamp(c.min_sample_rate(), c.max_sample_rate()), c))
        .collect();

    // Exact match first; then the closest rate at or above the source (so we
    // upsample rather than throw away bandwidth); then simply the closest.
    options.sort_by_key(|(r, _)| {
        let exact = *r != rate;
        let below = *r < rate;
        let dist = r.abs_diff(rate);
        (exact, below, dist)
    });

    // GMUSE_RATE forces a device rate, to measure resampling cost in isolation.
    let forced = std::env::var("GMUSE_RATE")
        .ok()
        .and_then(|v| v.parse::<u32>().ok());
    let (chosen_rate, range) = match forced {
        Some(f) => candidates
            .iter()
            .find(|c| c.min_sample_rate() <= f && f <= c.max_sample_rate())
            .map(|c| (f, c))
            .ok_or_else(|| format!("device cannot be opened at {f} Hz"))?,
        None => options[0],
    };

    let mut config: StreamConfig = range.with_sample_rate(chosen_rate).into();
    if let SupportedBufferSize::Range { min, max } = range.buffer_size() {
        config.buffer_size = cpal::BufferSize::Fixed((*min).max(512).min(*max));
    }
    Ok(config)
}

/// Push interleaved frames into the ring, waiting for room.
///
/// Runs on the decoder thread, so blocking here is fine — the RT callback is
/// never blocked, it just reads whatever is available.
fn push_all(producer: &mut rtrb::Producer<f32>, data: &[f32]) {
    let mut offset = 0;
    while offset < data.len() {
        let free = producer.slots();
        if free == 0 {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        }
        let n = free.min(data.len() - offset);
        match producer.write_chunk_uninit(n) {
            Ok(chunk) => {
                chunk.fill_from_iter(data[offset..offset + n].iter().copied());
                offset += n;
            }
            Err(_) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
}
