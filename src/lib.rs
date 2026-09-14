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

// **These live in `gmuse-library` and are named here as they always were** —
// #213. The binary, the UI and the Python door all say `crate::model::…` and
// `crate::query::…` in forty thousand places; re-exporting at the old paths is
// what makes the move a crate boundary instead of a rewrite of every caller.
#[cfg(test)]
pub use gmuse_library::fixture;
pub use gmuse_library::{
    audio_id, bridge, cache, events, facts, history, librarian, model, next, operations, playlist,
    projection, query, shelf, text,
};

// **The audio path lives in `gmuse-engine` and is named here as it always was**
// — #213, the same trick the librarian's modules use above.
pub use gmuse_engine::audio;
pub mod audit;
pub mod cli;
pub mod cmd;
pub mod config;
pub mod datadir;
pub mod edit;
pub mod engine;
pub mod guides;
// The pidfile lock lives in `gmuse-engine`, named here as it always was — #213.
pub use gmuse_engine::lock;
pub mod m3u;
pub mod session;
/// The view bench, rehoused by #213 — see the module header.
#[cfg(test)]
pub mod shelf_perf;
pub mod smart;
pub mod ui;
pub mod undo;
pub mod view;
pub mod wiring;

/// **The Python door**, behind a feature so an ordinary build needs no
/// interpreter — #197.
#[cfg(feature = "python")]
pub mod python;
