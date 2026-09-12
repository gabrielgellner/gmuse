//! **The named operations — the only things that touch the filesystem.**
//!
//! `access-layers.md` §2: the filesystem is not a layer. It is reached by
//! operations a person names — scan, import, verify, render, retire, apply —
//! and by nothing else. Those sit **beside** the layers rather than inside
//! them: they take a read handle and the roots, they read bytes, and they
//! append facts.
//!
//! ## Why they needed a home
//!
//! They did not have one. `storage` and `scan` and `apply` sat at the crate
//! root and `populate` sat inside `src/model/`, doing 32 filesystem operations
//! from a directory whose whole rule is that it is the domain and knows about
//! nothing else. The layering held because of how those files happened to be
//! written, which is the kind of thing that holds until it does not.
//!
//! [`boundary`] asserts it now, in both directions: **these may open a file
//! and nothing else may**, and they read downward and never reach an adapter.
//! That turns three separate "no `std::fs` here" tests into one positive rule
//! with somewhere to live.
//!
//! ## What each one is
//!
//! | | |
//! | --- | --- |
//! | [`scan`] | what this device holds, as the player's cache sees it |
//! | [`populate`] | the walk behind `shelf scan` and `shelf import` — mints, gates (#160), publishes holdings |
//! | [`storage`] | where a file goes, and moving it there — `render` and `retire` |
//! | [`apply`] | a file of proposed facts, read and applied |
//!
//! **None of them decides anything a person did not ask for.** A scan
//! reconciles holdings and reads no tags; import is a declaration; render
//! moves what the layout says and refuses what it cannot place.

pub mod apply;
pub mod populate;
pub mod scan;
pub mod storage;

#[cfg(test)]
mod boundary;
