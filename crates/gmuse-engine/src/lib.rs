//! **The audio path, and the engine that drives it** — #213, §4.3.
//!
//! Everything between a decision and a sound: the RT callback, the decoder, the
//! resampler, the control thread that owns the `!Send` player handle, and the
//! socket that lets something other than a TUI make the decision.
//!
//! ## What is not here
//!
//! The command language. §4.3's table puts the control thread in this crate and
//! the TUI in the root, and the control thread used to match on `cmd::Command` —
//! a vocabulary with `Up`, `Fold` and `PaneNext` in it, most of which it had to
//! refuse. A crate cannot depend on the thing that depends on it, so the split
//! forced that question rather than letting it stand.

pub mod audio;
pub mod engine;
pub mod instruction;
/// **The pidfile lock**, here because the engine is what needs one.
///
/// Plain infrastructure rather than anything about sound, and it sits in this crate
/// for the ordinary reason: `engine::claim` cannot reach up into the root, and the
/// root can reach down here. The bare player's own lock in `main` uses it through
/// the same door.
pub mod lock;
/// The allocation guard, in the test binary only — #211.
#[cfg(test)]
pub mod rt_guard;
#[cfg(test)]
#[global_allocator]
static RT_GUARD: rt_guard::Guarded = rt_guard::Guarded;
