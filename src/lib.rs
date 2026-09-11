//! **gmuse as a library**, so something other than the binary can hold it —
//! #197.
//!
//! The crate was a binary and nothing else, which is why the Python tools talk
//! to it through files: there was no other door. A `cdylib` needs a library
//! target, so the modules move here and `main.rs` becomes what it always
//! described itself as — an adapter, parsing a command line and calling in.
//!
//! Nothing else changes. `access-layers.md`'s layers are the same modules in
//! the same order, and the boundary tests still walk the same directories.

pub mod audio;
pub mod audio_id;
pub mod audit;
pub mod bridge;
pub mod cache;
pub mod cli;
pub mod cmd;
pub mod config;
pub mod datadir;
pub mod edit;
pub mod events;
pub mod facts;
#[cfg(test)]
pub mod fixture;
pub mod guides;
pub mod history;
pub mod librarian;
pub mod lock;
pub mod m3u;
pub mod model;
pub mod next;
pub mod operations;
pub mod playlist;
pub mod projection;
pub mod query;
/// The allocation guard, in the test binary only — #211.
#[cfg(test)]
pub mod rt_guard;
#[cfg(test)]
#[global_allocator]
static RT_GUARD: rt_guard::Guarded = rt_guard::Guarded;
pub mod remote;
pub mod session;
pub mod shelf;
pub mod smart;
pub mod text;
pub mod ui;
pub mod undo;
pub mod view;

/// **The Python door**, behind a feature so an ordinary build needs no
/// interpreter — #197.
#[cfg(feature = "python")]
pub mod python;
