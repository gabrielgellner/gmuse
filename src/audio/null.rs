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

use super::device::{self, Rt};

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

        let thread = std::thread::Builder::new()
            .name("gmuse-null-audio".into())
            .spawn(move || {
                while !stopping.load(Ordering::Relaxed) {
                    device::process(&mut rt, &mut out);
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
