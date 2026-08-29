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
use super::{PlayerCmd, PlayerEvent, SeekTo, Shared, State, TrackSource};

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

/// How long to wait for the answer to `NeedNext` before finishing the track.
///
/// Sized between the two numbers that bound it: comfortably more than the UI's
/// 200ms poll, so an idle loop always gets to answer, and comfortably less than
/// the ring's 1000ms, so a track that is *not* answered for still finishes
/// cleanly. Missing the window costs a gap — today's behaviour — not a fault.
const NEXT_REPLY_TIMEOUT: Duration = Duration::from_millis(500);

/// The currently loaded track.
struct Track {
    format: Box<dyn FormatReader>,
    decoder: Box<dyn symphonia::core::codecs::audio::AudioDecoder>,
    track_id: u32,
    source_rate: u32,
    channels: usize,
    /// Linear gain for this track's samples — loudness normalisation. Applied
    /// as frames are produced, so it travels with the audio rather than being
    /// applied at the output, where a gapless join would put it a second early.
    gain: f32,
    resampler: Option<Resampler>,
    /// Device frames pushed into the ring since the last flush.
    pushed_frames: u64,
    /// Device-frame position the ring's contents start at.
    base_frames: u64,
    /// Set once the reader has run out of packets.
    drained: bool,
    /// Set once the resampler tail has been pushed and nothing follows, so
    /// all that is left is to wait for the ring to play out.
    finishing: bool,
    /// The head of the track, in *source* frames: the probe packet `open`
    /// decoded to learn the rate (see the note there).
    ///
    /// Deliberately not resampled at open time. A gapless splice inherits the
    /// previous track's filter, and which filter the head goes through is not
    /// settled until that has happened — running it through this track's own
    /// filter first would discard that filter's delay and swallow the frames.
    head: Vec<f32>,
    /// Device-rate frames that must be emitted before any further decoding:
    /// the tail of the packet a seek landed inside. Dropping it would silently
    /// round the seek forward to the next packet boundary.
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
    // The reply to `NeedNext`: `None` = not answered yet, `Some(None)` =
    // answered "nothing follows". Tri-state because "no answer" and "no next
    // track" lead to different behaviour and must not be confused.
    let mut next_answer: Option<Option<TrackSource>> = None;
    let mut asked: Option<std::time::Instant> = None;
    // `Loaded` events held back until the playhead reaches the join they
    // describe. More than one can be outstanding only for a track shorter
    // than the ring is deep, which is why this is a queue and not an Option.
    let mut boundaries: std::collections::VecDeque<(u64, PlayerEvent)> =
        std::collections::VecDeque::new();

    loop {
        // A spliced track becomes real when the playhead reaches it, not when
        // the decoder got there — a ring's depth earlier. Checked first so a
        // command handled below acts on the track the listener is hearing.
        publish_crossed(&mut boundaries, &shared, &events);

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
                PlayerCmd::SetNext(path) => {
                    next_answer = Some(path);
                    continue;
                }
                PlayerCmd::Load(source) => {
                    outbox.clear();
                    // A load supersedes whatever was spliced on: its own
                    // `Loaded` follows, so a held-back one would be a lie.
                    boundaries.clear();
                    next_answer = None;
                    asked = None;
                    shared.draining.store(false, Ordering::Release);
                    flush(&shared, 0);
                    match open(
                        &source.path,
                        &codecs,
                        device_rate,
                        device_channels,
                        quality,
                        source.gain,
                    ) {
                        Ok(t) => {
                            let duration = track_duration(&t);
                            let _ = events.try_send(PlayerEvent::Loaded {
                                path: source.path,
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
                    // Whatever was spliced on is the track that stops, so its
                    // `Loaded` is owed even though its audio never played.
                    publish_all(&mut boundaries, &events);
                    next_answer = None;
                    asked = None;
                    shared.draining.store(false, Ordering::Release);
                    flush(&shared, 0);
                    if let Some(t) = track.as_mut() {
                        t.pushed_frames = 0;
                        t.base_frames = 0;
                        t.drained = false;
                        t.finishing = false;
                        // Rewind so a later Play starts from the top.
                        let _ = seek_to_frame(t, 0, device_rate);
                    }
                    continue;
                }
                PlayerCmd::Seek(to) => {
                    // Same reasoning as `Stop`: seeking acts on the spliced
                    // track, so the caller must be told it is the one playing.
                    publish_all(&mut boundaries, &events);
                    if let Some(t) = track.as_mut() {
                        let now = shared
                            .position_frames
                            .load(Ordering::Relaxed)
                            .saturating_sub(shared.track_base.load(Ordering::Relaxed));
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
                                // Seeking backwards out of the tail means the
                                // track is no longer ending.
                                shared.draining.store(false, Ordering::Release);
                                flush(&shared, landed);
                                t.base_frames = landed;
                                t.pushed_frames = 0;
                                t.drained = false;
                                t.finishing = false;
                                next_answer = None;
                                asked = None;
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

        if t.drained && !t.finishing {
            // The track is decoded to its end but the ring still holds ~1s of
            // it. That second is the budget for splicing the next track on
            // without a gap (DESIGN §11.1). Ask now, and decide on the answer.
            match next_answer.take() {
                Some(Some(source)) => {
                    match open(
                        &source.path,
                        &codecs,
                        device_rate,
                        device_channels,
                        quality,
                        source.gain,
                    ) {
                        Ok(mut nt) => {
                            // Carry the resampler when the ratio is unchanged.
                            // Flushing it would zero-pad the final chunk, and
                            // that padding is up to 1024 frames of silence
                            // spliced into the join — the very gap M7 removes.
                            // Fed continuously instead, the filter resamples
                            // the two tracks as the single signal they are.
                            if nt.source_rate == t.source_rate && nt.channels == t.channels {
                                nt.resampler = t.resampler.take();
                            } else if let Some(r) = t.resampler.as_mut() {
                                // Different ratio, so the filter cannot carry.
                                // Its tail is still the end of this track and
                                // has to go out before the switch.
                                let mut tail = Vec::new();
                                if r.flush(&mut tail).is_ok() && !tail.is_empty() {
                                    // Still the outgoing track's audio, so it
                                    // takes the outgoing track's gain.
                                    scale(&mut tail, t.gain);
                                    let written = push(producer, &tail, &cmds);
                                    t.pushed_frames += (written / device_channels) as u64;
                                    if written < tail.len() {
                                        outbox.extend_from_slice(&tail[written..]);
                                    }
                                }
                            }
                            // Only now is the last of this track accounted
                            // for: the branch above may have pushed a tail.
                            let boundary = t.base_frames + t.pushed_frames;
                            nt.base_frames = boundary;
                            boundaries.push_back((
                                boundary,
                                PlayerEvent::Loaded {
                                    path: source.path,
                                    duration: track_duration(&nt),
                                    source_rate: nt.source_rate,
                                    channels: nt.channels,
                                    resampling: nt.resampler.is_some(),
                                },
                            ));
                            asked = None;
                            *t = nt;
                            continue;
                        }
                        Err(e) => {
                            // Unplayable: say so and end the current track
                            // normally, so the caller can move past it.
                            let _ = events.try_send(PlayerEvent::Error(e.to_string()));
                        }
                    }
                }
                Some(None) => {}
                None => {
                    if asked.is_none() {
                        let _ = events.try_send(PlayerEvent::NeedNext);
                        asked = Some(std::time::Instant::now());
                    }
                    if asked.is_some_and(|t| t.elapsed() < NEXT_REPLY_TIMEOUT) {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }
            }

            // Nothing follows: drain the filter tail and let the ring run out.
            let mut tail = Vec::new();
            if let Some(r) = t.resampler.as_mut()
                && r.flush(&mut tail).is_ok()
                && !tail.is_empty()
            {
                scale(&mut tail, t.gain);
                let written = push(producer, &tail, &cmds);
                t.pushed_frames += (written / device_channels) as u64;
                if written < tail.len() {
                    outbox.extend_from_slice(&tail[written..]);
                }
            }
            t.finishing = true;
            shared.draining.store(true, Ordering::Release);
            continue;
        }

        if t.finishing {
            // Everything is pushed; wait for the ring to play out.
            let played = shared.position_frames.load(Ordering::Relaxed);
            if played >= t.base_frames + t.pushed_frames {
                shared.state.store(State::Stopped as u8, Ordering::Relaxed);
                let _ = events.try_send(PlayerEvent::Finished);
                track = None;
                asked = None;
                next_answer = None;
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
                // End of stream. The resampler is deliberately *not* flushed
                // here: whether its tail is drained or carried into the next
                // track is the whole question, and it is settled below.
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

/// Emit the `Loaded` for any join the playhead has now reached.
///
/// A gapless splice happens a ring's depth — about a second — before the
/// listener hears it. Announcing it at splice time would put the wrong title
/// on screen for that second and make the position jump early, so the event
/// waits here for the audio to catch up.
fn publish_crossed(
    boundaries: &mut std::collections::VecDeque<(u64, PlayerEvent)>,
    shared: &Shared,
    events: &Sender<PlayerEvent>,
) {
    while let Some(&(at, _)) = boundaries.front() {
        if shared.position_frames.load(Ordering::Relaxed) < at {
            break;
        }
        // Before the event, so a position read prompted by it is already
        // relative to the track the event names.
        shared.track_base.store(at, Ordering::Release);
        let (_, event) = boundaries.pop_front().expect("front was just checked");
        let _ = events.try_send(event);
    }
}

/// Emit every held-back `Loaded` immediately, whatever the playhead is doing.
///
/// Used when a command acts on the spliced track before its audio is reached:
/// the caller is about to seek or stop *that* track, so it has to be told the
/// track changed even though the join was never played.
fn publish_all(
    boundaries: &mut std::collections::VecDeque<(u64, PlayerEvent)>,
    events: &Sender<PlayerEvent>,
) {
    for (_, event) in boundaries.drain(..) {
        let _ = events.try_send(event);
    }
}

/// Ask the callback to discard buffered audio and adopt `position`.
///
/// Blocks until the callback acknowledges, so the caller knows no stale audio
/// survives and the position is already correct. Costs one callback period.
fn flush(shared: &Shared, position: u64) {
    // A flush always establishes a new origin: `position` is measured from the
    // start of the track, so the gapless offset no longer applies.
    shared.track_base.store(0, Ordering::Release);
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

/// Scale frames by a track's gain. A no-op at unity, which is the common case.
fn scale(frames: &mut [f32], gain: f32) {
    if gain == 1.0 {
        return;
    }
    for sample in frames {
        *sample *= gain;
    }
}

/// Decode one packet, resampled to the device rate. `Ok(None)` at end of stream.
///
/// Everything it hands back is already scaled by the track's gain, so there is
/// one place frames become audible and one place the gain is applied.
fn decode_chunk(t: &mut Track) -> Result<Option<Vec<f32>>, DecodeError> {
    // The tail of the packet a seek landed inside comes out first.
    if !t.pending.is_empty() {
        let mut out = std::mem::take(&mut t.pending);
        scale(&mut out, t.gain);
        return Ok(Some(out));
    }

    // Then the head kept back by `open`, resampled now that the filter is
    // settled — inherited across a gapless join, or this track's own.
    if !t.head.is_empty() {
        let raw = std::mem::take(&mut t.head);
        let mut out = match t.resampler.as_mut() {
            Some(r) => {
                let mut out = Vec::new();
                r.process(&raw, &mut out)?;
                out
            }
            None => raw,
        };
        scale(&mut out, t.gain);
        return Ok(Some(out));
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

    let mut out = match t.resampler.as_mut() {
        Some(r) => {
            let mut converted = Vec::new();
            r.process(&raw, &mut converted)?;
            converted
        }
        None => raw,
    };
    scale(&mut out, t.gain);
    Ok(Some(out))
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
    t.head.clear();
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
    gain: f32,
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
    //
    // The audio from that packet is *kept*, not rewound past. Rewinding would
    // be the obvious way to get back to the start, but a decoder applies its
    // encoder-delay trim only while it is at the head of the stream: seeking
    // to zero and resetting hands back an untrimmed first packet. Measured on
    // Opus, whose 312-frame pre-skip survives as decoder ramp-up — inaudible
    // under the transport fade, but not under a gapless join, which has none.
    let (source_rate, channels, first) = loop {
        match format.next_packet() {
            Ok(Some(p)) if p.track_id != track_id => continue,
            Ok(Some(p)) => match decoder.decode(&p) {
                Ok(buf) => {
                    let spec = buf.spec();
                    let mut raw = vec![0.0f32; buf.samples_interleaved()];
                    buf.copy_to_slice_interleaved(&mut raw[..]);
                    break (spec.rate(), spec.channels().count(), raw);
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
        gain,
        resampler,
        pushed_frames: 0,
        base_frames: 0,
        drained: false,
        finishing: false,
        head: first,
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
    use std::sync::atomic::AtomicBool;

    /// A 16-bit PCM WAV whose every sample is a distinct function of its frame
    /// index, so a misaligned seek is unmistakable rather than merely plausible.
    fn write_test_wav(path: &Path, frames: u32, channels: u16, rate: u32) {
        write_seeded_wav(path, frames, channels, rate, 0);
    }

    /// The same, but `seed` shifts the whole pattern. Two files with different
    /// seeds share no samples, so a join between them can be located exactly:
    /// a dropped or duplicated frame moves the boundary, and inserted silence
    /// shows up as zeroes that neither file contains.
    fn write_seeded_wav(path: &Path, frames: u32, channels: u16, rate: u32, seed: u32) {
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
                w.extend_from_slice(&pattern(i, c, seed).to_le_bytes());
            }
        }
        std::fs::write(path, w).unwrap();
    }

    /// The sample at frame `i`, channel `c`, of a file written with `seed`.
    fn pattern(i: u32, c: u32, seed: u32) -> i16 {
        (((i.wrapping_mul(7919)
            .wrapping_add(c * 104_729)
            .wrapping_add(seed.wrapping_mul(31_337)))
            % 30_000) as i32
            - 15_000) as i16
    }

    /// What a file written by `write_seeded_wav` *contains*, as the decoder
    /// would hand it over.
    ///
    /// Derived from the generator, not from `open`, so it is independent of
    /// the code under test. Building the reference with `decode_chunk` instead
    /// would let a bug that drops audio break the expectation in exactly the
    /// same way and pass — which is precisely what happened when the head of
    /// the track went missing.
    fn expected_pcm(frames: u32, channels: u16, seed: u32) -> Vec<f32> {
        let mut out = Vec::with_capacity((frames * channels as u32) as usize);
        for i in 0..frames {
            for c in 0..channels as u32 {
                out.push(f32::from(pattern(i, c, seed)) / 32_768.0);
            }
        }
        out
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
        let mut linear = open(&path, &codecs, rate, ch, Quality::Balanced, 1.0).unwrap();
        let all = take_frames(&mut linear, TARGET + WINDOW, ch);
        let expected = &all[TARGET * ch..(TARGET + WINDOW) * ch];

        // Same window, reached by seeking.
        let mut seeked = open(&path, &codecs, rate, ch, Quality::Balanced, 1.0).unwrap();
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
        let mut linear = open(&path, &codecs, rate, ch, Quality::Balanced, 1.0).unwrap();
        let all = take_frames(&mut linear, 100_100, ch);

        for &target in &[1usize, 4_097, 44_099, 44_101, 99_999] {
            let mut t = open(&path, &codecs, rate, ch, Quality::Balanced, 1.0).unwrap();
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

    /// Run the decoder thread against a consumer that drains the ring and
    /// advances the position counter the way the callback does, and return
    /// every sample that came out plus the events that were reported.
    ///
    /// This is the only way to test a transition honestly: the splice, the
    /// boundary accounting and the held-back `Loaded` are all interactions
    /// between the two threads, and none of them is visible from `open` or
    /// `decode_chunk` alone.
    fn play_through(
        tracks: &[PathBuf],
        rate: u32,
        channels: usize,
        quality: Quality,
    ) -> (Vec<f32>, Vec<PlayerEvent>) {
        let sources: Vec<TrackSource> = tracks.iter().cloned().map(TrackSource::plain).collect();
        play_sources(&sources, rate, channels, quality)
    }

    fn play_sources(
        sources: &[TrackSource],
        rate: u32,
        channels: usize,
        quality: Quality,
    ) -> (Vec<f32>, Vec<PlayerEvent>) {
        let (samples, events) = play_through_paced(sources, rate, channels, quality, None);
        (samples, events.into_iter().map(|(e, _)| e).collect())
    }

    /// `pace` caps how many frames the sink takes per millisecond. Without it
    /// the sink drains the ring the instant anything lands, so the playhead
    /// tracks the decoder exactly and any claim about *when* something happens
    /// relative to the buffered audio becomes untestable. With it the ring
    /// fills, and the two run a ring's depth apart as they do in the player.
    ///
    /// Each event is paired with the playback position when it arrived.
    fn play_through_paced(
        tracks: &[TrackSource],
        rate: u32,
        channels: usize,
        quality: Quality,
        pace: Option<usize>,
    ) -> (Vec<f32>, Vec<(PlayerEvent, u64)>) {
        let (mut producer, mut consumer) = super::super::ring::new(rate, channels);
        let shared = Arc::new(Shared::new());
        let (cmd_tx, cmd_rx) = crossbeam_channel::bounded::<PlayerCmd>(64);
        let (evt_tx, evt_rx) = crossbeam_channel::bounded::<PlayerEvent>(64);

        let dec_shared = Arc::clone(&shared);
        let decoder = std::thread::spawn(move || {
            run(
                cmd_rx,
                evt_tx,
                &mut producer,
                dec_shared,
                rate,
                channels,
                quality,
            );
        });

        // Stand in for the callback: take whatever is there and account for it.
        let out = Arc::new(std::sync::Mutex::new(Vec::<f32>::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let sink = {
            let (out, stop, shared) = (Arc::clone(&out), Arc::clone(&stop), Arc::clone(&shared));
            std::thread::spawn(move || {
                let mut ack = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    // Honour the flush handshake, or a `Load` would block.
                    let seq = shared.flush_seq.load(Ordering::Acquire);
                    if seq != ack {
                        while consumer.pop().is_ok() {}
                        shared.position_frames.store(
                            shared.flush_position.load(Ordering::Relaxed),
                            Ordering::Relaxed,
                        );
                        ack = seq;
                        shared.flush_ack.store(seq, Ordering::Release);
                        continue;
                    }
                    let mut n = consumer.slots();
                    if let Some(max) = pace {
                        n = n.min(max * channels);
                    }
                    n -= n % channels;
                    if n == 0 {
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                    let chunk = consumer.read_chunk(n).expect("slots were just counted");
                    let (a, b) = chunk.as_slices();
                    out.lock().unwrap().extend_from_slice(a);
                    out.lock().unwrap().extend_from_slice(b);
                    chunk.commit_all();
                    shared
                        .position_frames
                        .fetch_add((n / channels) as u64, Ordering::Relaxed);
                    if pace.is_some() {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                }
            })
        };

        let mut events = Vec::new();
        let mut queue: Vec<TrackSource> = tracks[1..].to_vec();
        queue.reverse();
        cmd_tx.send(PlayerCmd::Load(tracks[0].clone())).unwrap();

        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            match evt_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PlayerEvent::NeedNext) => {
                    let next = queue.pop();
                    cmd_tx.send(PlayerCmd::SetNext(next)).unwrap();
                }
                Ok(PlayerEvent::Finished) => {
                    events.push((
                        PlayerEvent::Finished,
                        shared.position_frames.load(Ordering::Relaxed),
                    ));
                    break;
                }
                Ok(e) => events.push((e, shared.position_frames.load(Ordering::Relaxed))),
                Err(_) if std::time::Instant::now() > deadline => panic!("decoder never finished"),
                Err(_) => {}
            }
        }

        cmd_tx.send(PlayerCmd::Shutdown).unwrap();
        decoder.join().unwrap();
        stop.store(true, Ordering::Relaxed);
        sink.join().unwrap();

        let samples = out.lock().unwrap().clone();
        (samples, events)
    }

    fn gapless_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gmuse-gapless-{name}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// The M7 acceptance criterion, stated as audio rather than as timing: the
    /// samples coming out of the ring are the two tracks concatenated, exactly,
    /// with nothing inserted and nothing lost.
    ///
    /// A gap would show up as zeroes at the join — neither file contains a
    /// zero-crossing pattern that could be mistaken for one — and a dropped or
    /// duplicated frame would shift everything after the boundary. Checking
    /// only "no silence" would pass while the join was one frame short, which
    /// is why this compares the whole stream.
    #[test]
    fn a_gapless_transition_splices_the_two_tracks_exactly() {
        let dir = gapless_dir("exact");
        let (rate, channels) = (44_100u32, 2usize);
        let (a_frames, b_frames) = (30_000u32, 20_000u32);
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_seeded_wav(&a, a_frames, channels as u16, rate, 1);
        write_seeded_wav(&b, b_frames, channels as u16, rate, 2);

        let (got, _) = play_through(&[a, b], rate, channels, Quality::Balanced);

        let mut expected = expected_pcm(a_frames, channels as u16, 1);
        expected.extend(expected_pcm(b_frames, channels as u16, 2));

        assert_eq!(
            got.len(),
            expected.len(),
            "stream is {} frames long, expected {}",
            got.len() / channels,
            expected.len() / channels
        );
        let mismatch = got.iter().zip(&expected).position(|(x, y)| x != y);
        assert_eq!(
            mismatch,
            None,
            "first differing frame {} (the join is at frame {})",
            mismatch.unwrap_or(0) / channels,
            a_frames
        );
    }

    /// A resampled join must be identical to resampling the two tracks as the
    /// single continuous signal they are.
    ///
    /// This is the criterion the resampler makes fiddly. Ending a track flushes
    /// the filter, which zero-pads the final chunk; starting the next one
    /// builds a fresh filter, which discards its own delay. Neither is audible
    /// as *silence* — the padding comes back out as filter ringing, so looking
    /// for runs of zeroes at the join finds nothing and passes while the bug is
    /// plainly there. Comparing against the continuous reference does not.
    #[test]
    fn a_resampled_transition_matches_resampling_the_two_tracks_as_one() {
        let dir = gapless_dir("resampled");
        let (src_rate, dev_rate, channels) = (44_100u32, 48_000u32, 2usize);
        let (a_frames, b_frames) = (30_000u32, 20_000u32);
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_seeded_wav(&a, a_frames, channels as u16, src_rate, 1);
        write_seeded_wav(&b, b_frames, channels as u16, src_rate, 2);

        let (got, _) = play_through(&[a, b], dev_rate, channels, Quality::Balanced);

        // The reference: both tracks' source samples through one filter, which
        // is what "gapless" means for a resampled stream.
        let mut src = expected_pcm(a_frames, channels as u16, 1);
        src.extend(expected_pcm(b_frames, channels as u16, 2));
        let mut r = Resampler::new(src_rate, dev_rate, channels, Quality::Balanced).unwrap();
        let mut expected = Vec::new();
        r.process(&src, &mut expected).unwrap();
        r.flush(&mut expected).unwrap();

        assert_eq!(
            got.len() / channels,
            expected.len() / channels,
            "resampled stream is the wrong length"
        );
        let mismatch = got.iter().zip(&expected).position(|(x, y)| x != y);
        assert_eq!(
            mismatch,
            None,
            "first differing frame {} — the join is near frame {}",
            mismatch.unwrap_or(0) / channels,
            (a_frames as f64 * dev_rate as f64 / src_rate as f64) as usize
        );
    }

    /// `Loaded` marks the moment a track becomes audible, so for a spliced
    /// track it must arrive when the playhead *reaches* the join — not when
    /// the decoder crossed it, a ring's depth earlier.
    ///
    /// Without this the title on screen changes about a second early and the
    /// elapsed time jumps before the audio does. The sink is paced here for
    /// that reason: draining the ring instantly would keep the decoder and the
    /// playhead in lockstep, and an event sent a second early would look
    /// identical to one sent on time.
    #[test]
    fn the_second_loaded_arrives_only_once_its_audio_has_played() {
        let dir = gapless_dir("timing");
        let (rate, channels) = (44_100u32, 2usize);
        // A must be longer than the ring is deep, or there is no lag to detect.
        let (a_frames, b_frames) = (66_150u32, 22_050u32);
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_seeded_wav(&a, a_frames, channels as u16, rate, 1);
        write_seeded_wav(&b, b_frames, channels as u16, rate, 2);

        // 20x real time: fast enough to keep the test short, slow enough that
        // the decoder stays a full ring ahead.
        let (got, events) = play_through_paced(
            &[TrackSource::plain(a), TrackSource::plain(b.clone())],
            rate,
            channels,
            Quality::Balanced,
            Some(rate as usize / 50),
        );

        let loaded: Vec<(&PathBuf, u64)> = events
            .iter()
            .filter_map(|(e, pos)| match e {
                PlayerEvent::Loaded { path, .. } => Some((path, *pos)),
                _ => None,
            })
            .collect();
        assert_eq!(loaded.len(), 2, "expected one Loaded per track: {events:?}");

        let (path, at) = loaded[1];
        assert_eq!(path, &b);
        assert!(
            at >= u64::from(a_frames),
            "the second Loaded arrived at frame {at}, {} frames before its own \
             audio started playing at {a_frames}",
            u64::from(a_frames) - at
        );

        // Exactly one Finished, at the very end — a splice is not an ending.
        let finished = events
            .iter()
            .filter(|(e, _)| matches!(e, PlayerEvent::Finished))
            .count();
        assert_eq!(finished, 1, "a spliced track must not report Finished");

        // And pacing must not have cost any audio.
        let mut expected = expected_pcm(a_frames, channels as u16, 1);
        expected.extend(expected_pcm(b_frames, channels as u16, 2));
        assert_eq!(got, expected, "paced playback lost or inserted samples");
    }

    /// A per-track gain must change exactly at the join, not when the decoder
    /// reaches it — those are a ring's depth apart after M7.
    ///
    /// This is why the gain is applied in the decoder rather than at the
    /// output: it travels with the frames it belongs to, so alignment is not
    /// something anyone has to maintain. Applying it in the callback would put
    /// the next track's gain on the last second of the current one, which no
    /// test of the gain's *value* would catch.
    #[test]
    fn a_spliced_track_takes_its_own_gain_from_the_join() {
        let dir = gapless_dir("gain");
        let (rate, channels) = (44_100u32, 2usize);
        let (a_frames, b_frames) = (30_000u32, 20_000u32);
        let a = dir.join("a.wav");
        let b = dir.join("b.wav");
        write_seeded_wav(&a, a_frames, channels as u16, rate, 1);
        write_seeded_wav(&b, b_frames, channels as u16, rate, 2);

        // A at unity, B at exactly half.
        let sources = vec![
            TrackSource { path: a, gain: 1.0 },
            TrackSource { path: b, gain: 0.5 },
        ];
        let (got, _) = play_sources(&sources, rate, channels, Quality::Balanced);

        let mut expected = expected_pcm(a_frames, channels as u16, 1);
        expected.extend(
            expected_pcm(b_frames, channels as u16, 2)
                .iter()
                .map(|s| s * 0.5),
        );

        assert_eq!(got.len(), expected.len(), "the join moved");
        let mismatch = got.iter().zip(&expected).position(|(x, y)| x != y);
        assert_eq!(
            mismatch,
            None,
            "first differing frame {} — the join is at frame {a_frames}, so a \
             gain applied early shows up before it and one applied late after",
            mismatch.unwrap_or(0) / channels
        );
    }

    /// Answering `NeedNext` with `None` is how playback stops, and it must
    /// still produce every sample of the last track.
    #[test]
    fn declining_to_splice_still_plays_the_track_out() {
        let dir = gapless_dir("decline");
        let (rate, channels) = (44_100u32, 2usize);
        let a = dir.join("a.wav");
        write_seeded_wav(&a, 30_000, channels as u16, rate, 1);

        let (got, events) = play_through(&[a], rate, channels, Quality::Balanced);

        let expected = expected_pcm(30_000, channels as u16, 1);
        assert_eq!(got, expected, "the last track was not played to its end");
        assert!(matches!(events.last(), Some(PlayerEvent::Finished)));
    }
}
