//! **Where the librarian and the audio engine are joined** — #257.
//!
//! The two resident actors name no library type of each other's: the engine holds a window
//! of cues and the librarian owns the log, and the vocabulary between them is
//! `gmuse-programme`. What is left over is *translation* — a `librarian::Changed` becoming a
//! `gmuse_proto::Change`, a published fold becoming a `library` event — and that has to
//! happen somewhere both a library type and a protocol type are legitimately visible.
//!
//! **This is that somewhere, and it is the root crate on purpose.** `gmuse-library` must not
//! depend on `gmuse-proto`, because the wire describes the library and an edge the other way
//! would let a fact be shaped by a message. `gmuse-engine` must not depend on
//! `gmuse-library` at all. The root crate already depends on both, and joining things is
//! what §4 gives it to do.
//!
//! So these two files are not homeless — they were in the wrong place, inside the audio
//! crate, which is why that crate could name `Touched` and `Changed`.

pub mod announcer;
pub mod change;
pub mod kept;
pub mod relay;
