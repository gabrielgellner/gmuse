//! The decoder thread: demux → decode → resample → ring.
//!
//! Everything expensive lives here. The thread owns the Symphonia reader and
//! decoder, the resampler, and the ring's producer end, and it is free to
//! block — only the callback is real-time.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::codecs::registry::CodecRegistry;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{
    FormatOptions, FormatReader, SeekMode, SeekTo as SymSeekTo, TrackType,
};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::units::{Time, Timestamp};

use super::resample::{Quality, Resampler};
use super::{PlayerCmd, PlayerEvent, SeekTo, Shared, State};

#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("could not open {path}: {source}")]
    Open {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("unsupported format: {0}")]
    Format(String),
    #[error("no audio track in file")]
    NoTrack,
    #[error("unsupported codec: {0}")]
    Codec(String),
    #[error("resampler: {0}")]
    Resample(#[from] super::resample::ResampleError),
    #[error("{0}")]
    Symphonia(String),
}

/// How long to wait for ring space before checking for commands again.
const BACKPRESSURE_POLL: Duration = Duration::from_millis(5);

/// The currently loaded track.
struct Track {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track_id: u32,
    source_rate: u32,
    channels: usize,
    resampler: Option<Resampler>,
    /// Device frames pushed into the ring since the last flush.
    pushed_frames: u64,
    /// Device-frame position the ring's contents start at.
    base_frames: u64,
    /// Set once the reader has run out of packets.
    drained: bool,
    /// Device-rate frames left over from the packet a seek landed inside.
    /// Must be emitted before any further decoding, or the seek would
    /// effectively round forward to the next packet boundary.
    pending: Vec<f32>,
}

pub(super) fn run(
    cmds: Receiver<PlayerCmd>,
    events: Sender<PlayerEvent>,
    producer: &mut rtrb::Producer<f32>,
    shared: Arc<Shared>,
    device_rate: u32,
    device_channels: usize,
    quality: Quality,
) {
    let codecs = codec_registry();
    let mut track: Option<Track> = None;
    // Device-rate frames decoded but not yet written to the ring, because a
    // command interrupted the write. Must survive until they are written, or
    // the stream has a hole in it.
    let mut outbox: Vec<f32> = Vec::new();

    loop {
        // Surface device state changes once each, as events.
        if shared.device_lost.swap(false, Ordering::AcqRel) {
            let _ = events.try_send(PlayerEvent::DeviceLost(
                "output stream reported an error".into(),
            ));
        }
        if shared.device_rerouted.swap(false, Ordering::AcqRel) {
            let _ = events.try_send(PlayerEvent::DeviceChanged);
        }

        let state = State::from_u8(shared.state.load(Ordering::Relaxed));
        let idle = track.is_none() || state == State::Stopped;

        // Idle: block for a command. Otherwise: take any pending command and
        // get back to decoding.
        let cmd = if idle {
            match cmds.recv() {
                Ok(c) => Some(c),
                Err(_) => return,
            }
        } else {
            match cmds.try_recv() {
                Ok(c) => Some(c),
                Err(crossbeam_channel::TryRecvError::Empty) => None,
                Err(crossbeam_channel::TryRecvError::Disconnected) => return,
            }
        };

        if let Some(cmd) = cmd {
            match cmd {
                PlayerCmd::Shutdown => return,
                PlayerCmd::Load(path) => {
                    outbox.clear();
                    flush(&shared, 0);
                    match open(&path, &codecs, device_rate, device_channels, quality) {
                        Ok(t) => {
                            let duration = track_duration(&t);
                            let _ = events.try_send(PlayerEvent::Loaded {
                                path,
                                duration,
                                source_rate: t.source_rate,
                                channels: t.channels,
                                resampling: t.resampler.is_some(),
                            });
                            track = Some(t);
                            shared.state.store(State::Playing as u8, Ordering::Relaxed);
                        }
                        Err(e) => {
                            track = None;
                            shared.state.store(State::Stopped as u8, Ordering::Relaxed);
                            let _ = events.try_send(PlayerEvent::Error(e.to_string()));
                        }
                    }
                    continue;
                }
                PlayerCmd::Play => {
                    if track.is_some() {
                        shared.state.store(State::Playing as u8, Ordering::Relaxed);
                    }
                }
                PlayerCmd::Pause => {
                    if state == State::Playing {
                        shared.state.store(State::Paused as u8, Ordering::Relaxed);
                    }
                }
                PlayerCmd::Toggle => {
                    let next = match state {
                        State::Playing => State::Paused,
                        _ if track.is_some() => State::Playing,
                        _ => State::Stopped,
                    };
                    shared.state.store(next as u8, Ordering::Relaxed);
                }
                PlayerCmd::Stop => {
                    shared.state.store(State::Stopped as u8, Ordering::Relaxed);
                    outbox.clear();
                    flush(&shared, 0);
                    if let Some(t) = track.as_mut() {
                        t.pushed_frames = 0;
                        t.base_frames = 0;
                        t.drained = false;
                        // Rewind so a later Play starts from the top.
                        let _ = seek_to_frame(t, 0, device_rate);
                    }
                    continue;
                }
                PlayerCmd::Seek(to) => {
                    if let Some(t) = track.as_mut() {
                        let now = shared.position_frames.load(Ordering::Relaxed);
                        let target = match to {
                            SeekTo::Absolute(d) => (d.as_secs_f64() * device_rate as f64) as i64,
                            SeekTo::Relative(secs) => {
                                now as i64 + (secs * device_rate as f64) as i64
                            }
                        }
                        .max(0) as u64;

                        match seek_to_frame(t, target, device_rate) {
                            Ok(landed) => {
                                outbox.clear();
                                flush(&shared, landed);
                                t.base_frames = landed;
                                t.pushed_frames = 0;
                                t.drained = false;
                            }
                            Err(e) => {
                                let _ = events.try_send(PlayerEvent::Error(e.to_string()));
                            }
                        }
                    }
                    continue;
                }
            }
            continue;
        }

        // No command pending. Decode if we should.
        let Some(t) = track.as_mut() else { continue };

        // Anything left over from an earlier write goes first, or it would be
        // a gap in the stream.
        if !outbox.is_empty() {
            let written = push(producer, &outbox, &cmds);
            outbox.drain(..written);
            t.pushed_frames += (written / device_channels) as u64;
            if !outbox.is_empty() && cmds.is_empty() {
                // The ring is full: wait for the callback to drain some. This
                // is the only place the decoder waits on backpressure.
                std::thread::sleep(BACKPRESSURE_POLL);
            }
            continue;
        }

        if t.drained {
            // Everything is pushed; wait for the ring to play out.
            let played = shared.position_frames.load(Ordering::Relaxed);
            if played >= t.base_frames + t.pushed_frames {
                shared.state.store(State::Stopped as u8, Ordering::Relaxed);
                let _ = events.try_send(PlayerEvent::Finished);
                track = None;
            } else {
                std::thread::sleep(BACKPRESSURE_POLL);
            }
            continue;
        }

        match decode_chunk(t) {
            Ok(Some(frames)) => {
                let written = push(producer, &frames, &cmds);
                t.pushed_frames += (written / device_channels) as u64;
                if written < frames.len() {
                    // Interrupted by a command: keep the rest for next time.
                    outbox.extend_from_slice(&frames[written..]);
                }
            }
            Ok(None) => {
                // End of stream: drain the resampler tail, then wait it out.
                let mut tail = Vec::new();
                if let Some(r) = t.resampler.as_mut()
                    && r.flush(&mut tail).is_ok()
                    && !tail.is_empty()
                {
                    let written = push(producer, &tail, &cmds);
                    t.pushed_frames += (written / device_channels) as u64;
                    if written < tail.len() {
                        outbox.extend_from_slice(&tail[written..]);
                    }
                }
                t.drained = true;
            }
            Err(e) => {
                shared.state.store(State::Stopped as u8, Ordering::Relaxed);
                let _ = events.try_send(PlayerEvent::Error(e.to_string()));
                track = None;
            }
        }
    }
}

/// Ask the callback to discard buffered audio and adopt `position`.
///
/// Blocks until the callback acknowledges, so the caller knows no stale audio
/// survives and the position is already correct. Costs one callback period.
fn flush(shared: &Shared, position: u64) {
    shared.flush_position.store(position, Ordering::Relaxed);
    let seq = shared.flush_seq.fetch_add(1, Ordering::Release) + 1;

    // The stream may not be running (device lost, or not yet started), so give
    // up rather than spin for ever.
    for _ in 0..200 {
        if shared.flush_ack.load(Ordering::Acquire) >= seq {
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    shared.position_frames.store(position, Ordering::Relaxed);
}

/// Push interleaved device-rate frames into the ring.
///
/// Returns `false` if a command is waiting, so the caller can go and service
/// it instead of sitting on backpressure — this is what makes `stop` and
/// `seek` feel immediate with a full second of audio buffered.
/// Write as much of `data` into the ring as fits, without blocking.
///
/// Returns how many samples were written. It stops short when the ring is full
/// or a command is waiting — the latter so `stop` and `seek` feel immediate
/// with a second of audio already buffered.
///
/// The caller **must** keep the remainder and write it next time. Dropping it
/// splices a gap into the stream, heard as a click whenever playback reaches
/// it, typically well after the command that caused it.
///
/// Non-blocking by design: waiting for the callback to make room belongs in
/// the run loop, where the wait can be interrupted, and it keeps this function
/// testable without a concurrent consumer.
fn push(producer: &mut rtrb::Producer<f32>, data: &[f32], cmds: &Receiver<PlayerCmd>) -> usize {
    let mut offset = 0;
    while offset < data.len() {
        // Never *receive* here: taking a command off the channel to detect it
        // would consume it, and the caller would never act on it. Peek only.
        if !cmds.is_empty() {
            break;
        }
        let free = producer.slots();
        if free == 0 {
            break;
        }
        let n = free.min(data.len() - offset);
        match producer.write_chunk_uninit(n) {
            Ok(chunk) => {
                chunk.fill_from_iter(data[offset..offset + n].iter().copied());
                offset += n;
            }
            Err(_) => break,
        }
    }
    offset
}

/// Decode one packet, resampled to the device rate. `Ok(None)` at end of stream.
fn decode_chunk(t: &mut Track) -> Result<Option<Vec<f32>>, DecodeError> {
    // The tail of the packet a seek landed inside comes out first.
    if !t.pending.is_empty() {
        return Ok(Some(std::mem::take(&mut t.pending)));
    }

    let mut raw: Vec<f32> = Vec::new();

    loop {
        let packet = match t.format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                return Ok(None);
            }
            Err(e) => return Err(DecodeError::Symphonia(e.to_string())),
        };
        if packet.track_id != t.track_id {
            continue;
        }
        match t.decoder.decode(&packet) {
            Ok(buf) => {
                raw.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut raw[..]);
                break;
            }
            // Skip corrupt packets rather than abandoning the track.
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
            Err(e) => return Err(DecodeError::Symphonia(e.to_string())),
        }
    }

    match t.resampler.as_mut() {
        Some(r) => {
            let mut out = Vec::new();
            r.process(&raw, &mut out)?;
            Ok(Some(out))
        }
        None => Ok(Some(raw)),
    }
}

/// Seek so that the next decoded frame is exactly `target` device frames in.
///
/// `SeekMode::Accurate` lands *at or before* the request, so to be truly
/// sample-accurate we decode forward from where it landed and throw away the
/// difference. Without this the position would be right but the audio would
/// start early, by up to a packet.
fn seek_to_frame(t: &mut Track, target: u64, device_rate: u32) -> Result<u64, DecodeError> {
    let seconds = target as f64 / device_rate as f64;
    let time = Time::try_from_secs_f64(seconds).unwrap_or(Time::ZERO);

    let seeked = t
        .format
        .seek(
            SeekMode::Accurate,
            SymSeekTo::Time {
                time,
                track_id: Some(t.track_id),
            },
        )
        .map_err(|e| DecodeError::Symphonia(e.to_string()))?;

    t.decoder.reset();
    t.pending.clear();
    if let Some(r) = t.resampler.as_mut() {
        r.reset();
    }

    // Convert where we landed back into source frames, and discard forward.
    let landed_src = seeked.actual_ts.get().max(0) as u64;
    let wanted_src = (seconds * t.source_rate as f64) as u64;
    let mut to_discard = wanted_src.saturating_sub(landed_src);

    while to_discard > 0 {
        let packet = match t.format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(_) => break,
        };
        if packet.track_id != t.track_id {
            continue;
        }
        match t.decoder.decode(&packet) {
            Ok(buf) => {
                let frames = buf.frames() as u64;
                if frames <= to_discard {
                    to_discard -= frames;
                } else {
                    // The seek landed inside this packet. Keep the tail of it:
                    // dropping it would silently round the seek forward to the
                    // next packet boundary, which is exactly the error this
                    // whole function exists to avoid.
                    let mut raw = vec![0.0f32; buf.samples_interleaved()];
                    buf.copy_to_slice_interleaved(&mut raw[..]);
                    let keep = &raw[(to_discard as usize * t.channels)..];
                    match t.resampler.as_mut() {
                        Some(r) => r.process(keep, &mut t.pending)?,
                        None => t.pending.extend_from_slice(keep),
                    }
                    to_discard = 0;
                }
            }
            Err(SymError::DecodeError(_)) | Err(SymError::IoError(_)) => continue,
            Err(e) => return Err(DecodeError::Symphonia(e.to_string())),
        }
    }

    Ok(target)
}

fn open(
    path: &Path,
    codecs: &CodecRegistry,
    device_rate: u32,
    device_channels: usize,
    quality: Quality,
) -> Result<Track, DecodeError> {
    let file = std::fs::File::open(path).map_err(|e| DecodeError::Open {
        path: path.to_path_buf(),
        source: e,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            mss,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|e| DecodeError::Format(e.to_string()))?;

    let track = format
        .default_track(TrackType::Audio)
        .ok_or(DecodeError::NoTrack)?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or(DecodeError::NoTrack)?
        .clone();

    let mut decoder = codecs
        .make_audio_decoder(&params, &AudioDecoderOptions::default())
        .map_err(|e| DecodeError::Codec(e.to_string()))?;

    // Codec parameters can omit the rate, but a decoded buffer always knows
    // its own spec, so decode one packet to find out for certain.
    let (source_rate, channels) = loop {
        match format.next_packet() {
            Ok(Some(p)) if p.track_id != track_id => continue,
            Ok(Some(p)) => match decoder.decode(&p) {
                Ok(buf) => {
                    let spec = buf.spec();
                    break (spec.rate(), spec.channels().count());
                }
                Err(SymError::DecodeError(_)) => continue,
                Err(e) => return Err(DecodeError::Symphonia(e.to_string())),
            },
            Ok(None) => return Err(DecodeError::Format("no decodable audio".into())),
            Err(e) => return Err(DecodeError::Symphonia(e.to_string())),
        }
    };

    if channels != device_channels {
        return Err(DecodeError::Format(format!(
            "{channels}-channel audio, device is {device_channels}-channel \
             (channel mapping is not implemented yet)"
        )));
    }

    // Rewind: the probe packet above already consumed audio.
    let _ = format.seek(
        SeekMode::Accurate,
        SymSeekTo::Time {
            time: Time::ZERO,
            track_id: Some(track_id),
        },
    );
    decoder.reset();

    let resampler = if source_rate == device_rate {
        None
    } else {
        Some(Resampler::new(source_rate, device_rate, channels, quality)?)
    };

    Ok(Track {
        format,
        decoder,
        track_id,
        source_rate,
        channels,
        resampler,
        pushed_frames: 0,
        base_frames: 0,
        drained: false,
        pending: Vec::new(),
    })
}

fn track_duration(t: &Track) -> Option<Duration> {
    let track = t.format.tracks().iter().find(|tr| tr.id == t.track_id)?;
    let tb = track.time_base?;
    let frames = track.num_frames?;
    let time = tb.calc_time(Timestamp::new(frames as i64))?;
    Some(Duration::from_secs_f64(time.as_secs_f64()))
}

/// Symphonia's enabled decoders, plus libopus when the `opus` feature is on.
fn codec_registry() -> CodecRegistry {
    let mut registry = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut registry);
    #[cfg(feature = "opus")]
    registry.register_audio_decoder::<symphonia_adapter_libopus::OpusDecoder>();
    registry
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV whose every sample is a distinct function of its frame
    /// index, so a misaligned seek is unmistakable rather than merely plausible.
    fn write_test_wav(path: &Path, frames: u32, channels: u16, rate: u32) {
        let bytes_per_sample = 2u16;
        let block_align = channels * bytes_per_sample;
        let data_len = frames * block_align as u32;

        let mut w: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        w.extend_from_slice(b"RIFF");
        w.extend_from_slice(&(36 + data_len).to_le_bytes());
        w.extend_from_slice(b"WAVEfmt ");
        w.extend_from_slice(&16u32.to_le_bytes());
        w.extend_from_slice(&1u16.to_le_bytes()); // PCM
        w.extend_from_slice(&channels.to_le_bytes());
        w.extend_from_slice(&rate.to_le_bytes());
        w.extend_from_slice(&(rate * block_align as u32).to_le_bytes());
        w.extend_from_slice(&block_align.to_le_bytes());
        w.extend_from_slice(&(bytes_per_sample * 8).to_le_bytes());
        w.extend_from_slice(b"data");
        w.extend_from_slice(&data_len.to_le_bytes());

        for i in 0..frames {
            for c in 0..channels as u32 {
                let v = (((i.wrapping_mul(7919).wrapping_add(c * 104_729)) % 30_000) as i32
                    - 15_000) as i16;
                w.extend_from_slice(&v.to_le_bytes());
            }
        }
        std::fs::write(path, w).unwrap();
    }

    /// Pull `n` device frames out of a track.
    fn take_frames(t: &mut Track, n: usize, channels: usize) -> Vec<f32> {
        let mut out = Vec::new();
        while out.len() < n * channels {
            match decode_chunk(t) {
                Ok(Some(chunk)) => out.extend_from_slice(&chunk),
                Ok(None) => break,
                Err(e) => panic!("decode failed: {e}"),
            }
        }
        out.truncate(n * channels);
        out
    }

    /// The M2 acceptance criterion: after seeking to frame N, the very next
    /// sample decoded is the one at frame N — not the start of whatever packet
    /// happens to contain it.
    ///
    /// `SeekMode::Accurate` only promises to land *at or before* the request,
    /// which for MP3-like formats can be a packet early. `seek_to_frame`
    /// decodes forward and discards the difference; this test is what makes
    /// that claim checkable.
    #[test]
    fn seek_is_sample_accurate() {
        let dir = std::env::temp_dir().join("gmuse-seek-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp.wav");
        let (rate, channels, frames) = (44_100u32, 2u16, 200_000u32);
        write_test_wav(&path, frames, channels, rate);

        let codecs = codec_registry();
        let ch = channels as usize;

        // Ground truth: decode linearly and keep 1000 frames from 123_456.
        const TARGET: usize = 123_456;
        const WINDOW: usize = 1_000;
        let mut linear = open(&path, &codecs, rate, ch, Quality::Balanced).unwrap();
        let all = take_frames(&mut linear, TARGET + WINDOW, ch);
        let expected = &all[TARGET * ch..(TARGET + WINDOW) * ch];

        // Same window, reached by seeking.
        let mut seeked = open(&path, &codecs, rate, ch, Quality::Balanced).unwrap();
        seek_to_frame(&mut seeked, TARGET as u64, rate).unwrap();
        let got = take_frames(&mut seeked, WINDOW, ch);

        assert_eq!(got.len(), expected.len(), "short read after seek");
        let mismatch = got.iter().zip(expected).position(|(a, b)| a != b);
        assert!(
            mismatch.is_none(),
            "seek landed off by at least one sample: first mismatch at sample {:?} \
             (got {:?}, want {:?})",
            mismatch,
            mismatch.map(|i| got[i]),
            mismatch.map(|i| expected[i]),
        );

        let _ = std::fs::remove_file(&path);
    }

    /// Seeking to a position no packet boundary lands on must still be exact.
    #[test]
    fn seek_to_odd_offsets_is_exact() {
        let dir = std::env::temp_dir().join("gmuse-seek-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("ramp2.wav");
        let (rate, channels, frames) = (44_100u32, 2u16, 120_000u32);
        write_test_wav(&path, frames, channels, rate);

        let codecs = codec_registry();
        let ch = channels as usize;
        let mut linear = open(&path, &codecs, rate, ch, Quality::Balanced).unwrap();
        let all = take_frames(&mut linear, 100_100, ch);

        for &target in &[1usize, 4_097, 44_099, 44_101, 99_999] {
            let mut t = open(&path, &codecs, rate, ch, Quality::Balanced).unwrap();
            seek_to_frame(&mut t, target as u64, rate).unwrap();
            let got = take_frames(&mut t, 16, ch);
            let expected = &all[target * ch..(target + 16) * ch];
            assert_eq!(got, expected, "seek to frame {target} was not exact");
        }

        let _ = std::fs::remove_file(&path);
    }
    // ---- the ring write path ---------------------------------------------

    use crossbeam_channel::bounded;

    /// An interrupted or backpressured write must lose nothing. Dropping the
    /// tail of a chunk splices a gap into the stream, heard as a click once
    /// playback reaches it — typically a beat *after* the command that caused
    /// it, since the ring holds about a second.
    #[test]
    fn no_audio_is_lost_across_interruptions_and_a_full_ring() {
        // Deliberately smaller than the data, so writes must be resumed.
        let (mut producer, mut consumer) = rtrb::RingBuffer::<f32>::new(64);
        let (tx, rx) = bounded::<PlayerCmd>(8);

        let data: Vec<f32> = (0..1000).map(|i| i as f32).collect();
        let mut outbox: Vec<f32> = Vec::new();
        let mut drained: Vec<f32> = Vec::new();

        // First write: a command is already waiting, so nothing goes in.
        tx.send(PlayerCmd::Pause).unwrap();
        let written = push(&mut producer, &data, &rx);
        outbox.extend_from_slice(&data[written..]);
        let _ = rx.recv();

        // Now alternate writing and draining, interrupting periodically, the
        // way a user hammering pause would.
        for round in 0..500 {
            if round % 7 == 3 {
                tx.send(PlayerCmd::Toggle).unwrap();
            }
            let n = push(&mut producer, &outbox, &rx);
            outbox.drain(..n);
            let _ = rx.try_recv();

            let slots = consumer.slots();
            if slots > 0
                && let Ok(chunk) = consumer.read_chunk(slots)
            {
                let (a, b) = chunk.as_slices();
                drained.extend_from_slice(a);
                drained.extend_from_slice(b);
                chunk.commit_all();
            }
            if outbox.is_empty() && consumer.slots() == 0 {
                break;
            }
        }

        assert_eq!(
            drained.len(),
            data.len(),
            "{} samples went missing",
            data.len() as i64 - drained.len() as i64
        );
        assert_eq!(drained, data, "samples were reordered or corrupted");
    }

    #[test]
    fn push_stops_short_rather_than_blocking_or_dropping() {
        let (mut producer, _consumer) = rtrb::RingBuffer::<f32>::new(128);
        let (tx, rx) = bounded::<PlayerCmd>(8);
        let data = vec![1.0f32; 500];

        // Ring smaller than the data: write what fits and report it, never
        // block and never silently discard.
        let written = push(&mut producer, &data, &rx);
        assert_eq!(written, 128, "should fill the ring exactly");

        // A waiting command preempts the write entirely.
        tx.send(PlayerCmd::Stop).unwrap();
        assert_eq!(push(&mut producer, &data, &rx), 0);
    }
}
