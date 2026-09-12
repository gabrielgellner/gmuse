//! **What crosses the socket** — #214, §7.
//!
//! The envelope, the handshake, and the error codes. Nothing here knows how the
//! engine is built or what it plays; §4.3 forbids it, and the dependency list
//! is what enforces that rather than a promise.
//!
//! ## Why this crate exists at all
//!
//! §7.6 wants ids on the wire to be *real domain identities* — `Id<Recording>`,
//! not a string that resembles one — while §4.3 forbids depending on the
//! librarian. `gmuse-ids` is what makes both true at once, and this crate is
//! the reason it was worth extracting.
//!
//! ## It is JSON-RPC 2.0, narrowed
//!
//! One object per line, and **no batches**. JSON-RPC permits a top-level array;
//! this protocol does not, and [`envelope::parse_line`] refuses one with a
//! `-32600` rather than ignoring it. Claiming conformance while silently
//! dropping batches would be worse than not claiming it.

pub mod envelope;
pub mod error;
pub mod hello;
pub mod version;

pub use envelope::{Id, Request, Response};
pub use error::RpcError;
pub use version::{MAJOR, PROTOCOL};
