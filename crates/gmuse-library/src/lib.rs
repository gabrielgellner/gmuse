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
pub mod model;
pub mod operations;
pub mod projection;
pub mod query;
pub mod shelf;
pub mod text;
