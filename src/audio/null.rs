//! Rendering with no audio device — #210.
//!
//! Drives the same [`device::process`] the cpal callback drives, from a
//! virtual clock. Not a stub: the renderer under test is the renderer that
//! ships, which is the only way a test about scheduling or starvation means
//! anything.
//!
//! ## The clock is virtual, and that is the point
//!
//! A real device calls back every `period` frames because that much time has
//! passed. Here the thread decides, so it can run at wall speed — which is
//! what a headless engine wants, since nothing downstream should notice — or
//! as fast as it can, which is what a test wants when it would otherwise
//! spend six real minutes playing a six-minute track.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::device::{self, RenderClock, Rt};

/// How fast the virtual clock runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// One buffer per buffer's worth of wall time, like a device.
    RealTime,
    /// As fast as the renderer will go. Playback is silent and instant, which
    /// is what a test wants from a six-minute track.
    Fast,
}

/// A running null renderer. Dropping it stops the thread.
pub struct Sink {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Sink {
    /// Start rendering into nothing.
    ///
    /// `period` is the buffer size in **frames**, the same quantity a device
    /// would pick: it decides the granularity everything downstream sees, so
    /// a test that cares about buffer-size independence varies it here.
    pub fn start(mut rt: Rt, channels: usize, rate: u32, period: usize, pace: Pace) -> Sink {
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);

        // Allocated once, outside the loop: the render step may not allocate,
        // and a buffer per callback would be exactly the thing the real
        // callback is forbidden from doing.
        let mut out = vec![0.0f32; period * channels];
        let per_buffer = std::time::Duration::from_secs_f64(period as f64 / f64::from(rate));

        // **The virtual clock is a clock, not a stub** (#212). A device reports
        // when it called back and when the audio will sound; so does this, from
        // the same arithmetic a device does — one buffer of wall time per
        // buffer, and one buffer of latency ahead of it. Under `Fast` the
        // wall clock is meaningless but the *stream* clock is not: a test
        // asserting on a sounding position needs it to advance per buffer, not
        // per second, or the answer depends on how quick the machine is.
        let per_buffer_nanos = u64::try_from(per_buffer.as_nanos()).unwrap_or(u64::MAX);
        let mut callback_nanos: u64 = 0;

        let thread = std::thread::Builder::new()
            .name("gmuse-null-audio".into())
            .spawn(move || {
                while !stopping.load(Ordering::Relaxed) {
                    device::process(
                        &mut rt,
                        &mut out,
                        RenderClock {
                            callback_nanos,
                            playback_nanos: callback_nanos.saturating_add(per_buffer_nanos),
                        },
                    );
                    callback_nanos = callback_nanos.saturating_add(per_buffer_nanos);
                    if pace == Pace::RealTime {
                        std::thread::sleep(per_buffer);
                    }
                }
            })
            .expect("null audio thread");

        Sink {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Sink {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{Shared, State};
    use std::sync::atomic::Ordering;

    const RATE: u32 = 48_000;
    const CHANNELS: usize = 2;
    const PERIOD: usize = 256;

    /// Wait for a condition the render thread is responsible for, or give up.
    ///
    /// Polling rather than sleeping a fixed time: under `Fast` the thread is
    /// as quick as the machine, and a fixed sleep is either flaky on a loaded
    /// one or slow on every other.
    /// The mapping, retried — see [`Shared::sounding`] on why a reader can
    /// momentarily lose to a writer that is publishing in a tight loop.
    fn mapping(shared: &Shared) -> crate::audio::Sounding {
        for _ in 0..2_000 {
            if let Some(map) = shared.sounding() {
                return map;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the renderer published no mapping at all");
    }

    fn until(mut done: impl FnMut() -> bool) -> bool {
        for _ in 0..2_000 {
            if done() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        false
    }

    /// **The virtual clock advances per buffer, not per second** — #212.
    ///
    /// A test asserting on a sounding position needs the stream clock to be a
    /// function of how much audio was rendered. If the null backend reported
    /// wall time, the answer under `Fast` would depend on how quick the
    /// machine is, and the test would be measuring the CPU rather than the
    /// renderer.
    #[test]
    fn the_null_backend_publishes_a_clock_that_tracks_the_audio() {
        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let shared = Arc::new(Shared::new());
        let rt = device::Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        assert_eq!(shared.generation(), 1, "starting a renderer opens a stream");

        let audio = vec![0.25f32; PERIOD * CHANNELS * 64];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let sink = Sink::start(rt, CHANNELS, RATE, PERIOD, Pace::Fast);

        assert!(
            until(|| shared.sounding().is_some_and(|m| m.frame >= PERIOD as u64)),
            "the renderer published a mapping past the first buffer"
        );
        let map = mapping(&shared);

        assert_eq!(
            map.generation, 1,
            "it belongs to the stream that is running"
        );
        assert_eq!(
            map.frame % PERIOD as u64,
            0,
            "every mapping names a buffer boundary"
        );

        // One buffer of latency, which is what this backend reports and what
        // the arithmetic below depends on.
        let per_buffer_nanos = (PERIOD as u64 * 1_000_000_000) / u64::from(RATE);
        assert_eq!(map.latency_nanos, per_buffer_nanos);

        // **The instant advances whether or not audio does**, and that is
        // correct rather than a defect worth asserting around. The thread
        // renders past the end of the fed audio, publishing silence: the frame
        // counter stalls because nothing is consumed, while the clock keeps
        // running because time passes. So the two are only in step while audio
        // flows — `a_published_instant_tracks_the_frames_while_audio_flows`
        // pins that relation deterministically, by driving the render step
        // directly rather than racing a thread.
        //
        // What holds here regardless is that the clock is ahead of the sound
        // by the latency, and never behind it.
        assert!(
            map.at_nanos >= per_buffer_nanos,
            "the first buffer already sounds one buffer after it was computed"
        );
        let earlier = map;
        assert!(
            until(|| shared
                .sounding()
                .is_some_and(|m| m.at_nanos > earlier.at_nanos)),
            "and the clock keeps running"
        );

        drop(sink);
    }

    /// **A second stream is a second generation, and the first one's mapping
    /// is refused** — #212.
    ///
    /// The device disappearing and coming back is the likeliest hardware event
    /// in this design. What must not happen is a consumer carrying on with an
    /// extrapolation built against a clock that has restarted at zero.
    #[test]
    fn a_restarted_stream_invalidates_the_previous_mapping() {
        let shared = Arc::new(Shared::new());

        let (mut producer, consumer) = crate::audio::ring::new(RATE, CHANNELS);
        let rt = device::Rt::new(consumer, Arc::clone(&shared), CHANNELS, RATE);
        let audio = vec![0.25f32; PERIOD * CHANNELS * 32];
        if let Ok(chunk) = producer.write_chunk_uninit(audio.len()) {
            chunk.fill_from_iter(audio.iter().copied());
        }
        shared.state.store(State::Playing as u8, Ordering::Relaxed);

        let first = Sink::start(rt, CHANNELS, RATE, PERIOD, Pace::Fast);
        assert!(
            until(|| shared.sounding().is_some_and(|m| m.frame >= PERIOD as u64)),
            "the first stream rendered"
        );
        let before = mapping(&shared);
        assert_eq!(before.generation, 1);

        // The device goes away and comes back: the old renderer stops, a new
        // one opens on the same `Shared`.
        drop(first);
        let (_p2, consumer2) = crate::audio::ring::new(RATE, CHANNELS);
        let rt2 = device::Rt::new(consumer2, Arc::clone(&shared), CHANNELS, RATE);

        assert_eq!(shared.generation(), 2, "the restart is a new generation");
        let stale = mapping(&shared);
        assert_eq!(
            stale.generation, 1,
            "and still says which stream it came from"
        );
        assert_ne!(
            stale.generation,
            shared.generation(),
            "so a consumer discards its extrapolation rather than \
             carrying on against a clock that restarted"
        );

        drop(Sink::start(rt2, CHANNELS, RATE, PERIOD, Pace::Fast));
    }
}
