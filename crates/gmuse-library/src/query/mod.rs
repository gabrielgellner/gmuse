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
//! ## Nothing here opens a file
//!
//! It was a ratchet while [`rows`] still walked the library, because nothing
//! else noticed a file that arrived — `:rescan` was a stub and a launch no
//! longer scans (#64). `shelf scan` is that door now (#196), so it is an
//! assertion: no `std::fs` in production code under this directory.
//!
//! A read is a function of the projection. The same records give the same
//! rows, on any machine, whatever sits on disk that nobody has declared.

pub mod filter;
pub mod format;
pub mod found;
pub mod sort;

mod rows;
pub use rows::*;

#[cfg(test)]
mod boundary;
