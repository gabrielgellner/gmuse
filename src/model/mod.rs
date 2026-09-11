//! The librarian's domain model — `docs/design/domain-model.md`.
//!
//! Separate from the player by construction: nothing here knows about audio,
//! terminals or key bindings, and nothing in `ui/` should reach past this
//! module to a `Scan` or a path. That seam is the point of the milestone this
//! module opens (#77).
//!
//! ## What is unused here, and why each thing is (#113)
//!
//! This module carried `#![allow(dead_code)]` with a promise: it comes off
//! once the player reads through the seam. Removing it produced **43** dead
//! items — a number nobody chose, because a blanket allow has no number to
//! choose. Whatever is unused now says so at its own site, with a reason, so
//! that the next unused item is a decision rather than a drift.
//!
//! **The attribute an item carries classifies it**, and the two are not
//! interchangeable:
//!
//! - `#[expect(dead_code, reason = …)]` — **nothing** references this, tests
//!   included. It is a declaration waiting for its first use. `expect` is
//!   right because it is self-retiring: the day something calls it, the
//!   expectation is unfulfilled and the build says so, and the attribute has
//!   to go. Seventeen of these, almost all of them the entities
//!   `domain-model.md` proposes and nothing mints.
//!
//! - `#[allow(dead_code, reason = …)]` — tests exercise it; no production
//!   caller does. `expect` cannot be used: under `cargo test` the item *is*
//!   used, so the expectation reports itself unfulfilled and a clean build
//!   becomes impossible. That is a real constraint, not a preference, and it
//!   is why the two attributes appear side by side.
//!
//! An `allow` sits on an `impl` only where nearly every item in it is unused
//! — `Arena`, `Vocabulary`, `Bpm`, `Spine`, `Provenance`. On `Instance`,
//! twelve of twenty-seven items are unused, so those twelve carry their own:
//! a blanket over the other fifteen would hide exactly what this change
//! exists to expose.

pub mod about;
pub mod arena;
pub mod basis;
pub mod clock;
pub mod durable;
pub mod encoding;
pub mod entity;
pub mod facet;
pub mod focus;
pub mod id;
pub mod ingest;
pub mod instance;
pub mod key;
pub mod log;
pub mod merge;
pub mod migrate;
pub mod mint;
pub mod ord;
pub mod populate;
pub mod projection;
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
pub mod view;
pub mod vocabulary;
