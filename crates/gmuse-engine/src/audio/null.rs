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
                let mut clock = Metronome::new(per_buffer, std::time::Instant::now());
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
                    if pace == Pace::RealTime
                        && let Some(wait) = clock.wait(std::time::Instant::now())
                    {
                        std::thread::sleep(wait);
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

/// **A deadline, not a delay** — #242.
///
/// The loop used to render a buffer and then `sleep(per_buffer)`, so a cycle cost a
/// period *plus* the work of filling it and the renderer ran at `period / (period +
/// work)`. Measured at **83% of real time** — a three-minute track took three and a
/// half — which broke the one claim `Pace::RealTime` makes for itself: *"a headless
/// engine is still playing for somebody, and nothing downstream should be able to tell
/// which backend it is."* It was tellable, with a stopwatch.
///
/// A device does not have this problem because its clock runs whether or not the
/// callback is quick. This is that: the deadline advances by exactly one period per
/// buffer, so the work is absorbed rather than added.
struct Metronome {
    period: std::time::Duration,
    /// When the buffer just rendered was due to finish.
    next: std::time::Instant,
}

impl Metronome {
    fn new(period: std::time::Duration, now: std::time::Instant) -> Self {
        Self { period, next: now }
    }

    /// How long to sleep after rendering a buffer, or `None` when already late.
    ///
    /// **Falling far behind gives up the backlog rather than bursting through it**,
    /// which is also what a device does: an underrun is silence and a counter, not a
    /// promise to play the missing audio faster later. Without the reset, a laptop
    /// resuming from sleep would render at full speed until it had caught up on every
    /// buffer it missed — minutes of audio at once, out of a renderer whose entire
    /// purpose is to behave like a device.
    fn wait(&mut self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.next += self.period;
        match self.next.checked_duration_since(now) {
            Some(wait) => Some(wait),
            None => {
                // More than eight periods late — about 85 ms at 512 frames and
                // 48 kHz — is a stall rather than a slow buffer.
                if now.duration_since(self.next) > self.period * 8 {
                    self.next = now;
                }
                None
            }
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
    use std::time::{Duration, Instant};

    const ONE_BUFFER: Duration = Duration::from_nanos(10_666_666);

    /// **The schedule does not drift with the work** — #242.
    ///
    /// The bug was `sleep(per_buffer)` *after* rendering, so a cycle cost a period plus
    /// the work and the renderer ran at 83% of real time. This asserts the property that
    /// fixes it: however long a buffer takes to fill, the next deadline is one period
    /// after the last one, not one period after now.
    ///
    /// **No stopwatch.** Every instant here is derived by adding a `Duration` to one
    /// `Instant::now()`, so the test measures arithmetic rather than the machine — the
    /// load-sensitive shape thrown away on 2026-09-13 for passing alone and failing
    /// inside `just ci`.
    #[test]
    fn the_deadline_absorbs_the_work_rather_than_adding_it() {
        let t0 = Instant::now();
        let mut clock = Metronome::new(ONE_BUFFER, t0);

        // A buffer that took 4 ms to fill: sleep the remainder, not a whole period.
        let work = Duration::from_millis(4);
        let first = clock.wait(t0 + work).expect("not late");
        assert_eq!(
            first,
            ONE_BUFFER - work,
            "it slept a period on top of the work"
        );

        // The next buffer starts where the last was *due*, so a slow one does not push
        // the schedule out. Woke on time, then spent 4 ms again.
        let second = clock.wait(t0 + ONE_BUFFER + work).expect("not late");
        assert_eq!(
            second,
            ONE_BUFFER - work,
            "the schedule drifted by the work"
        );

        // **The instrument is attached.** Both assertions above hold trivially if
        // `wait` simply returned `ONE_BUFFER - work` while ignoring its argument, so this
        // varies the work and requires the answer to change with it.
        let slower = Duration::from_millis(9);
        let third = clock.wait(t0 + ONE_BUFFER * 2 + slower).expect("not late");
        assert_eq!(third, ONE_BUFFER - slower);
        assert_ne!(
            third, second,
            "the sleep does not depend on how long the work took"
        );
    }

    /// **A buffer that overran is late, not owed** — #242.
    ///
    /// Work longer than a period means there is nothing left to sleep. The renderer
    /// carries on immediately, which is the honest answer and what lets it catch up a
    /// buffer or two.
    #[test]
    fn a_buffer_that_overran_does_not_sleep() {
        let t0 = Instant::now();
        let mut clock = Metronome::new(ONE_BUFFER, t0);
        assert_eq!(
            clock.wait(t0 + ONE_BUFFER * 2),
            None,
            "it slept while already late"
        );
        // **And the time is made back, rather than lost.** The deadline is still on the
        // original schedule, so the next buffer — rendered instantly, as it would be
        // after an overrun — is exactly on time and sleeps zero rather than a period.
        // A schedule rebased to `now` would hand back a full period here and the
        // renderer would be permanently one buffer behind.
        assert_eq!(clock.wait(t0 + ONE_BUFFER * 2), Some(Duration::ZERO));
    }

    /// **A long stall gives up the backlog** — #242.
    ///
    /// A device that missed a second of audio does not then play a second of audio at
    /// double speed; it underruns, counts it, and carries on from now. Without this a
    /// laptop resuming from sleep would render every missed buffer at full tilt.
    #[test]
    fn a_stall_resets_the_schedule_rather_than_bursting() {
        let t0 = Instant::now();
        let mut clock = Metronome::new(ONE_BUFFER, t0);

        // Gone for a hundred periods.
        let woke = t0 + ONE_BUFFER * 100;
        assert_eq!(
            clock.wait(woke),
            None,
            "nothing to sleep while a hundred behind"
        );

        // **The next buffer is paced again**, rather than ninety-nine more racing to
        // catch up. That is the whole difference, and it is what this asserts: a
        // schedule that had not reset would return `None` here too.
        let after = clock
            .wait(woke)
            .expect("the schedule reset, so this one paces");
        assert!(
            after > Duration::ZERO && after <= ONE_BUFFER,
            "expected one period of pacing, got {after:?}"
        );
    }
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
