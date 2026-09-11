//! **L2 — the query.** A pure function of a projection.
//!
//! `access-layers.md` §2: given a projection this layer is a function, with no
//! I/O and no clock it was not handed. Same filter, same levels, same fields,
//! whether a person typed them on the `:` line or a script passed `--filter`.
//!
//! ## What this directory may not import, and why it is a test
//!
//! A documented boundary rots. `boundary::the_query_layer_reads_no_files`
//! walks these files' `use` lines and fails on a forbidden one, which is the
//! mechanism `model/seam.rs` already uses and which has caught two live
//! mistakes there.
//!
//! The load-bearing entries are `walkdir`, `lofty` and `symphonia`. A read path
//! that opens a music file is the defect this layer exists to prevent: it is
//! how an `artist=` filter returning four rows came to parse the tags of 7,267
//! files, and how the answer to a question came to depend on what a tagger last
//! wrote rather than on what the library knows.
//!
//! ## `std::fs` is a ratchet here, not yet an assertion
//!
//! [`rows`] still walks the library, because nothing else notices a file that
//! arrived — `:rescan` is a stub and a launch no longer scans (#64). So the
//! test asserts **no worse than today** and the count may only fall. #195 takes
//! it to zero, together with #196 which gives the filesystem its own door.

pub mod filter;
pub mod format;
pub mod sort;

mod rows;
pub use rows::*;

#[cfg(test)]
mod boundary;
