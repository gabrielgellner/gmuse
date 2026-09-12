//! What version of the protocol this is, and what a mismatch means — §7.3.

use std::fmt;

/// The protocol this engine speaks.
///
/// **`gmuse/N`, and `N` is the only part that decides compatibility.** Minor
/// features are advertised through `capabilities` in the handshake rather than
/// by bumping this, so a client that understands `gmuse/0` keeps working as
/// methods are added — it simply does not ask for what it was not told about.
pub const PROTOCOL: &str = "gmuse/0";

/// The major version this build speaks.
pub const MAJOR: u32 = 0;

/// A protocol string that did not parse, or named a different major version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionError {
    /// Not of the form `gmuse/N`.
    Malformed(String),
    /// Well-formed, and not a version this engine can speak.
    Incompatible { theirs: u32, ours: u32 },
}

impl fmt::Display for VersionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(s) => write!(f, "not a protocol version: {s:?}, expected \"gmuse/N\""),
            Self::Incompatible { theirs, ours } => {
                write!(
                    f,
                    "client speaks gmuse/{theirs}, this engine speaks gmuse/{ours}"
                )
            }
        }
    }
}

impl std::error::Error for VersionError {}

/// The major version in a `gmuse/N` string.
pub fn major_of(protocol: &str) -> Result<u32, VersionError> {
    protocol
        .strip_prefix("gmuse/")
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| VersionError::Malformed(protocol.to_string()))
}

/// Whether this engine will talk to a client announcing `protocol`.
///
/// **A differing major closes the connection** (§7.3). Not negotiated down: a
/// client built against a different shape of the envelope would misread every
/// message after this one, and failing at the handshake is the only point where
/// the failure is still legible.
pub fn accept(protocol: &str) -> Result<(), VersionError> {
    let theirs = major_of(protocol)?;
    if theirs == MAJOR {
        Ok(())
    } else {
        Err(VersionError::Incompatible {
            theirs,
            ours: MAJOR,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_engine_speaks_the_version_it_advertises() {
        assert!(accept(PROTOCOL).is_ok());
        assert_eq!(major_of(PROTOCOL), Ok(MAJOR));
    }

    #[test]
    fn a_different_major_is_refused_rather_than_negotiated() {
        assert_eq!(
            accept("gmuse/1"),
            Err(VersionError::Incompatible { theirs: 1, ours: 0 })
        );
    }

    /// Not `gmuse/0.1`, not `gmuse`, not a bare number. The refusal names what
    /// was expected, because the likeliest sender of one of these is somebody
    /// hand-writing a client at a terminal.
    #[test]
    fn anything_that_is_not_gmuse_n_is_malformed() {
        for bad in ["gmuse", "gmuse/", "gmuse/0.1", "0", "", "mpd/0", "gmuse/-1"] {
            assert!(
                matches!(accept(bad), Err(VersionError::Malformed(_))),
                "{bad:?} should be malformed"
            );
        }
    }
}
