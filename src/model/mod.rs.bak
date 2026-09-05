//! The librarian's domain model — `docs/design/domain-model.md`.
//!
//! Separate from the player by construction: nothing here knows about audio,
//! terminals or key bindings, and nothing in `ui/` should reach past this
//! module to a `Scan` or a path. That seam is the point of the milestone this
//! module opens (#77).
//!
//! The layer is built bottom-up, ahead of the callers that will use it, so its
//! items are unused until the seam lands. `dead_code` is allowed here for that
//! reason and **comes off with #77** — if it is still needed after the player
//! reads through this module, something did not get wired up.
#![allow(dead_code)]

pub mod arena;
pub mod basis;
pub mod clock;
pub mod durable;
pub mod encoding;
pub mod entity;
pub mod id;
pub mod ingest;
pub mod instance;
pub mod log;
pub mod merge;
pub mod migrate;
pub mod mint;
pub mod populate;
pub mod proposal;
pub mod provenance;
pub mod qualifier;
pub mod reference;
pub mod round_trip;
pub mod rows;
pub mod seam;
pub mod sortkey;
pub mod spine;
pub mod tree;
pub mod vocabulary;
