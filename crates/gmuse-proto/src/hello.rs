//! The first message on a connection — §7.3.

use serde::{Deserialize, Serialize};

/// What a client says about itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloParams {
    /// `gmuse/N`. A differing major closes the connection.
    pub protocol: String,
    /// For logs and for telling two views apart. Not authentication.
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub client_version: String,
}

/// What the engine answers with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HelloResult {
    pub protocol: String,
    /// **Random, chosen at engine start.** It scopes every reference that is
    /// not stable across a restart (§7.6): a client holding one from a previous
    /// engine must be told, rather than having it silently mean something else.
    pub engine_generation: String,
    /// **Which audio stream the frame clock is counting in** (§10.1, #212).
    ///
    /// A device disappearing kills the cpal stream and its replacement starts
    /// from zero, so a client extrapolating a position must discard it when
    /// this changes. Zero before any stream has opened — under `--null-audio`
    /// before the renderer starts, or while a device is being reacquired.
    pub stream_generation: u32,
    /// The engine's clock now, for estimating the offset (§7.9).
    pub engine_now_ns: i64,
    /// The device's rate. Everything scheduled in frames is in this rate.
    pub sample_rate: u32,
    /// **Minor features, so the protocol version does not have to move.** A
    /// client asks only for what it was told about here.
    pub capabilities: Vec<String>,
}

impl HelloParams {
    /// A hello from this build, for a client naming itself.
    pub fn new(client: &str, client_version: &str) -> Self {
        Self {
            protocol: crate::version::PROTOCOL.into(),
            client: client.into(),
            client_version: client_version.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The transcript in §7.3 is the contract. If this stops parsing, either
    /// the doc or the struct has moved without the other.
    #[test]
    fn the_documented_hello_parses() {
        let params: HelloParams = serde_json::from_str(
            r#"{"protocol":"gmuse/0","client":"nvim","client_version":"0.1.0"}"#,
        )
        .unwrap();
        assert_eq!(params.protocol, "gmuse/0");
        assert_eq!(params.client, "nvim");
    }

    /// A client that names no version still connects: `client` is for logs, not
    /// for authentication, so refusing over it would be refusing for no reason.
    #[test]
    fn a_client_that_says_only_its_protocol_is_enough() {
        let params: HelloParams = serde_json::from_str(r#"{"protocol":"gmuse/0"}"#).unwrap();
        assert_eq!(params.client, "");
    }

    #[test]
    fn the_documented_result_round_trips() {
        let result = HelloResult {
            protocol: crate::version::PROTOCOL.into(),
            engine_generation: "8f3a".into(),
            stream_generation: 3,
            engine_now_ns: 123_456_789_012_345,
            sample_rate: 48_000,
            capabilities: vec!["dj".into()],
        };
        let line = serde_json::to_string(&result).unwrap();
        assert_eq!(serde_json::from_str::<HelloResult>(&line).unwrap(), result);
    }
}
