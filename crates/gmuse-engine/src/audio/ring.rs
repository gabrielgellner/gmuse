//! The decoder → callback ring buffer, and how deep it is.

/// Milliseconds of audio the ring holds when full.
///
/// DESIGN §3 asks for 500–1000ms. Deeper means more tolerance for a slow
/// decode or a scheduling hiccup; it also means a `stop` has more audio to
/// discard, which the flush handshake handles, and more latency before a
/// gapless transition can be prepared.
const DEPTH_MS: u32 = 1000;

pub(super) fn new(device_rate: u32, channels: usize) -> (rtrb::Producer<f32>, rtrb::Consumer<f32>) {
    let frames = (device_rate as usize * DEPTH_MS as usize) / 1000;
    rtrb::RingBuffer::new(frames * channels)
}
