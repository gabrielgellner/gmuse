//! **The librarian** — #213, §4.3.
//!
//! The log, the projection it folds to, the query language over it, and the
//! named operations that append to it. Everything that knows what the library
//! *is*, and nothing that knows how it is shown or played.
//!
//! ## What the boundary is, and what enforces it
//!
//! `the_model_does_not_know_the_player_exists` used to assert this by scanning
//! `use` lines for `crate::ui`, `crate::audio` and friends. It still runs, and
//! it is now the weaker of the two checks: ratatui, crossterm and cpal are not
//! dependencies of this crate, so reaching for them does not fail a test — it
//! fails to build, and adding them to `Cargo.toml` is the visible act.
//!
//! ## Why `bridge` is here despite its name
//!
//! It reads as the seam to the player, and it is the other way round:
//! `bridge::librarian` takes holdings and records and returns a `Librarian`.
//! Library types in, library type out. The player calls it; that does not make
//! it the player's.
//!
//! ## Why `shelf` is here despite being a subcommand's name
//!
//! `gmuse shelf copies` is dispatch, and dispatch is the binary's. This module
//! is the arrangement those subcommands report on — no clap, no argument
//! parsing, nothing that knows a terminal exists.

pub mod audio_id;
pub mod bridge;
pub mod cache;
pub mod events;
pub mod facts;
#[cfg(any(test, feature = "testing"))]
pub mod fixture;
pub mod history;
pub mod librarian;
/// **The pidfile lock** — #218.
///
/// Here because the thing with a single appender is the log, and the log is this
/// crate's. `gmuse-engine`'s socket lock is the same primitive through a re-export.
/// **The process, socket and append locks** — moved to `gmuse-lock` by #257.
///
/// Re-exported because `gmuse::lock::acquire` is what `main` says, and because the
/// librarian's `Appending::claim` is one of the three callers that made it a crate.
pub use gmuse_lock as lock;
pub mod model;
pub mod next;
pub mod operations;
pub mod playlist;
/// **What plays next** — `gmuse-programme` owns it now (#257).
///
/// Re-exported because the librarian is still where a programme is *answered from*: the
/// ordering lives in the crate both actors can name, and resolving a row to a path needs the
/// fold, which only this side has.
pub use gmuse_programme as programme;
pub mod projection;
pub mod query;
pub mod shelf;
pub mod text;

/// **What the fold costs in bytes** — #255. See the module docs for why an allocator rather
/// than a profiler.
///
/// **Behind `testing` as well as `cfg(test)`** — the same reason `row_of_test` is: a
/// measurement of the *engine's* startup cannot run in this crate's test binary, and an
/// `examples/` target is a separate crate that cannot reach `cfg(test)` items. A release build
/// still pays nothing, because nothing enables the feature.
#[cfg(any(test, feature = "testing"))]
pub mod accounting;

/// **Installed for the test binary only**, so a release build pays nothing for it.
///
/// The same shape as `gmuse-engine`'s `rt_guard`: that one proves the render step
/// allocates nothing, this one says how much the fold allocates.
#[cfg(test)]
#[global_allocator]
static ACCOUNTING: accounting::Accounting = accounting::Accounting;
