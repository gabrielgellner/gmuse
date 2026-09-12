//! What a file's audio is called, independent of anything a tagger does.
//!
//! **The type, not its derivation.** Computing an `AudioId` means probing a
//! file and hashing its packets, which needs a decoder; and this crate is the
//! one `gmuse-proto` depends on, so a decoder here would put symphonia on the
//! wire crate's dependency graph. `gmuse::audio_id` keeps `measure` and `of`
//! and the reasoning about what is hashed; what crosses a wire or names a row
//! is this.

use std::fmt;

#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    Hash,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
pub struct AudioId {
    pub scheme: u8,
    pub hash: u128,
}

impl fmt::Display for AudioId {
    /// `v1:` then 32 hex digits — greppable, and it says what made it.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "v{}:{:032x}", self.scheme, self.hash)
    }
}

impl std::str::FromStr for AudioId {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let rest = s.strip_prefix('v').ok_or(())?;
        let (scheme, hash) = rest.split_once(':').ok_or(())?;
        Ok(AudioId {
            scheme: scheme.parse().map_err(|_| ())?,
            hash: u128::from_str_radix(hash, 16).map_err(|_| ())?,
        })
    }
}
