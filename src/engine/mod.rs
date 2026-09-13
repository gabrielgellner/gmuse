//! **The view's side of the engine** — #213.
//!
//! The engine itself is `gmuse-engine`: the control thread, the socket, the audio
//! path. What is left here is the half a *client* needs — connecting to one, and
//! starting one if none answers, which holds a `gmuse_client::Client` and knows
//! about §4.2's tmux model.
//!
//! Everything the engine crate exports is named through here too, so a caller that
//! said `gmuse::engine::claim` still says it.

pub mod attach;

pub use gmuse_engine::engine::*;
