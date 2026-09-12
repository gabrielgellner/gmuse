//! **One typed value per line** — I4, §7.5.
//!
//! Every ingress parses into typed domain values exactly once, at its edge, and
//! nothing downstream handles untyped data. [`Request::of`] is that edge for the
//! socket: after it, `method` is not a string anyone matches on again and
//! `params` is not a `Value` anyone reaches into.
//!
//! ## Why the mapping is a function rather than a serde attribute
//!
//! §7.5 draws `Envelope { id, request }` with the request "decoded from
//! `{method, params}`", which reads like `#[serde(tag = "method", content =
//! "params")]` flattened into the envelope. Serde's `flatten` buffers into its
//! own content type and does not compose with adjacent tagging, and the
//! workarounds are worse than the thing they avoid: what matters to I4 is that
//! there is **one place** where a string becomes a type, not which mechanism
//! spells it. This is that place, and it can say why a method was refused —
//! which an attribute cannot.
//!
//! ## What is not here yet, and why that is not an oversight
//!
//! §7.4's table is illustrative and names `deck.*`, `lane.*`, `param.set` and
//! `mapping.*`. Their payloads are `DeckId`, `When`, `Lane` and `BindingTarget`
//! — types that arrive with phases 6 to 8. Declaring the variants now would mean
//! inventing those types to hold nothing, and **a method name is public API**
//! (§7.4): shipping `deck.load` that accepts a shape nobody has designed is
//! harder to correct than not shipping it.

use serde::{Deserialize, Serialize};

use crate::error::RpcError;
use crate::hello::HelloParams;

/// What a client asked for, once it is a type.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// The handshake. Must be first on a connection (§7.3).
    Hello(HelloParams),
    /// The cmus-style command language, parsed engine-side (§7.12).
    ///
    /// **The line stays a string here on purpose.** Its parser is the `:`
    /// line's — one textual command language, which is the standing complaint
    /// against `--repl` — and that parser belongs to the engine, not to a wire
    /// crate that must not depend on it (§4.3). The boundary is real; it is one
    /// layer in.
    ExecLine { line: String },
    /// What the engine is doing.
    EngineStatus,
    /// Ask it to stop.
    EngineShutdown,
    /// The engine's clock, for offset estimation (§7.9).
    TimeNow,
    /// Start receiving events on these topics (§7.8).
    Subscribe { topics: Vec<crate::Topic> },
    /// Stop receiving them.
    Unsubscribe { topics: Vec<crate::Topic> },
}

/// Which topics a subscription is about.
///
/// **An absent or empty list means every topic.** A client that asks for
/// everything is the common case — a status bar wants whatever moves — and
/// making it enumerate seven names it would have to update when an eighth
/// arrives is how clients fall behind the protocol.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TopicsParams {
    #[serde(default)]
    pub topics: Vec<crate::Topic>,
}

impl TopicsParams {
    pub fn resolved(self) -> Vec<crate::Topic> {
        if self.topics.is_empty() {
            crate::Topic::ALL.to_vec()
        } else {
            self.topics
        }
    }
}

/// The typed parameter blocks, so each one has a name a refusal can quote.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecLineParams {
    pub line: String,
}

/// The method names, in one place.
///
/// **Public API** (§7.4): renaming one is a protocol version bump, so they are
/// constants rather than string literals scattered through a match.
pub mod method {
    pub const HELLO: &str = "hello";
    pub const EXEC_LINE: &str = "cmd.exec_line";
    pub const ENGINE_STATUS: &str = "engine.status";
    pub const ENGINE_SHUTDOWN: &str = "engine.shutdown";
    pub const TIME_NOW: &str = "time.now";
    pub const SUBSCRIBE: &str = "sub.subscribe";
    pub const UNSUBSCRIBE: &str = "sub.unsubscribe";

    /// Every method this engine answers. Used by `engine.status` and by the
    /// test that keeps the golden transcripts honest.
    pub const ALL: &[&str] = &[
        HELLO,
        EXEC_LINE,
        ENGINE_STATUS,
        ENGINE_SHUTDOWN,
        TIME_NOW,
        SUBSCRIBE,
        UNSUBSCRIBE,
    ];
}

impl Request {
    /// Turn a method and its params into a typed request.
    ///
    /// **The one parse boundary for the socket.** An unknown method is a
    /// `-32601` and a malformed payload is a `-32602` naming the field, so a
    /// caller never has to guess which of the two it hit.
    pub fn of(method: &str, params: Option<serde_json::Value>) -> Result<Self, RpcError> {
        fn typed<T: serde::de::DeserializeOwned>(
            method: &str,
            params: Option<serde_json::Value>,
        ) -> Result<T, RpcError> {
            // **Absent params are an empty object, not null.**
            //
            // A method whose fields are all optional should accept being called
            // with nothing — `sub.subscribe` documents exactly that, and the
            // first version refused it with "invalid type: null, expected
            // struct TopicsParams", which contradicted the doc comment two
            // screens above. Found by writing the golden transcript for it.
            //
            // Null is retried as `{}` rather than mapped up front so that a
            // method genuinely wanting a null payload still sees one.
            let value = params.unwrap_or(serde_json::Value::Null);
            let retry = value.is_null();
            serde_json::from_value(value)
                .or_else(|e| {
                    if retry {
                        serde_json::from_value(serde_json::Value::Object(Default::default()))
                            .map_err(|_| e)
                    } else {
                        Err(e)
                    }
                })
                .map_err(|e| RpcError::invalid_params("params", format!("{method}: {e}")))
        }

        match method {
            method::HELLO => Ok(Self::Hello(typed(method, params)?)),
            method::EXEC_LINE => {
                let p: ExecLineParams = typed(method, params)?;
                Ok(Self::ExecLine { line: p.line })
            }
            method::ENGINE_STATUS => Ok(Self::EngineStatus),
            method::ENGINE_SHUTDOWN => Ok(Self::EngineShutdown),
            method::TIME_NOW => Ok(Self::TimeNow),
            method::SUBSCRIBE => {
                let p: TopicsParams = typed(method, params)?;
                Ok(Self::Subscribe {
                    topics: p.resolved(),
                })
            }
            method::UNSUBSCRIBE => {
                let p: TopicsParams = typed(method, params)?;
                Ok(Self::Unsubscribe {
                    topics: p.resolved(),
                })
            }
            other => Err(RpcError::method_not_found(other)),
        }
    }

    /// The name this request arrived under.
    pub fn method(&self) -> &'static str {
        match self {
            Self::Hello(_) => method::HELLO,
            Self::ExecLine { .. } => method::EXEC_LINE,
            Self::EngineStatus => method::ENGINE_STATUS,
            Self::EngineShutdown => method::ENGINE_SHUTDOWN,
            Self::TimeNow => method::TIME_NOW,
            Self::Subscribe { .. } => method::SUBSCRIBE,
            Self::Unsubscribe { .. } => method::UNSUBSCRIBE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::code;

    fn of(method: &str, params: serde_json::Value) -> Result<Request, RpcError> {
        Request::of(method, Some(params))
    }

    #[test]
    fn every_named_method_parses_and_reports_its_own_name() {
        let cases = [
            (method::HELLO, serde_json::json!({"protocol":"gmuse/0"})),
            (method::EXEC_LINE, serde_json::json!({"line":"pause"})),
            (method::ENGINE_STATUS, serde_json::json!(null)),
            (method::ENGINE_SHUTDOWN, serde_json::json!(null)),
            (method::TIME_NOW, serde_json::json!(null)),
        ];
        for (name, params) in cases {
            let request = of(name, params).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            assert_eq!(request.method(), name);
        }
    }

    /// `method::ALL` is what `engine.status` advertises, so a method that
    /// parses and is not listed would be undiscoverable, and one listed that
    /// does not parse would be a lie.
    #[test]
    fn the_advertised_list_is_exactly_what_parses() {
        for name in method::ALL {
            assert!(
                !matches!(
                    Request::of(name, Some(serde_json::json!({"protocol":"gmuse/0","line":"pause"}))),
                    Err(ref e) if e.code == code::METHOD_NOT_FOUND
                ),
                "{name} is advertised but not handled"
            );
        }
        assert_eq!(method::ALL.len(), 7, "a new method needs adding to ALL");
    }

    /// Asking for nothing asks for everything, so a client does not have to
    /// track the topic list to keep receiving what it always did.
    #[test]
    fn an_empty_topic_list_means_every_topic() {
        let request = of(method::SUBSCRIBE, serde_json::json!({})).unwrap();
        assert_eq!(
            request,
            Request::Subscribe {
                topics: crate::Topic::ALL.to_vec()
            }
        );
    }

    /// Absent params, not merely an empty object: `sub.subscribe` with nothing
    /// at all is the shape a client hand-typing at a socket sends.
    #[test]
    fn subscribing_with_no_params_at_all_means_every_topic() {
        assert_eq!(
            Request::of(method::SUBSCRIBE, None),
            Ok(Request::Subscribe {
                topics: crate::Topic::ALL.to_vec()
            })
        );
    }

    #[test]
    fn a_named_topic_list_is_taken_as_given() {
        let request = of(method::SUBSCRIBE, serde_json::json!({"topics":["queue"]})).unwrap();
        assert_eq!(
            request,
            Request::Subscribe {
                topics: vec![crate::Topic::Queue]
            }
        );
    }

    #[test]
    fn an_unknown_topic_is_invalid_params_rather_than_being_ignored() {
        let e = of(method::SUBSCRIBE, serde_json::json!({"topics":["weather"]})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
    }

    #[test]
    fn an_unknown_method_is_not_found_rather_than_bad_params() {
        let e = of("deck.load", serde_json::json!({})).unwrap_err();
        assert_eq!(e.code, code::METHOD_NOT_FOUND);
        assert!(e.message.contains("deck.load"), "{}", e.message);
    }

    /// **The two failures are told apart.** A client that sent the wrong shape
    /// to a real method and one that invented a method need different fixes.
    #[test]
    fn a_known_method_with_the_wrong_shape_is_invalid_params() {
        let e = of(method::EXEC_LINE, serde_json::json!({"lien":"pause"})).unwrap_err();
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data.unwrap()["field"], "params");
    }

    #[test]
    fn a_method_that_takes_nothing_does_not_mind_absent_params() {
        assert_eq!(Request::of(method::TIME_NOW, None), Ok(Request::TimeNow));
    }

    /// A line is carried verbatim: it is parsed by the command language, and
    /// mangling it here would be a second parser in the place I4 forbids one.
    #[test]
    fn an_exec_line_is_carried_through_untouched() {
        let request = of(
            method::EXEC_LINE,
            serde_json::json!({"line":"  seek +10  "}),
        )
        .unwrap();
        assert_eq!(
            request,
            Request::ExecLine {
                line: "  seek +10  ".into()
            }
        );
    }
}

/// **Malformed input produces errors, never panics** — #215's standing
/// requirement for every parse boundary.
///
/// A hand-rolled generator rather than a fuzzing dependency: §12 says not to
/// add one without a Decision Log entry, and what this needs is coverage of the
/// shapes that actually reach a boundary — truncations of valid input, wrong
/// types in the right fields, and bytes that are not JSON at all. A random byte
/// string almost never gets past `serde_json` and so tests the same branch
/// repeatedly.
#[cfg(test)]
mod no_input_panics {
    use super::*;
    use crate::envelope::parse_line;

    /// A cheap deterministic sequence. Seeded per-case so a failure is
    /// reproducible from the test name alone.
    fn xorshift(state: &mut u64) -> u64 {
        *state ^= *state << 13;
        *state ^= *state >> 7;
        *state ^= *state << 17;
        *state
    }

    const SEEDS: &[&str] = &[
        r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/0"}}"#,
        r#"{"jsonrpc":"2.0","id":1,"method":"cmd.exec_line","params":{"line":"seek +10"}}"#,
        r#"{"jsonrpc":"2.0","method":"time.now"}"#,
        r#"{"jsonrpc":"2.0","id":"x","method":"engine.status","params":null}"#,
    ];

    /// Every prefix of a valid request. Truncation is what a closed socket and
    /// a half-flushed writer both look like.
    #[test]
    fn no_truncation_of_a_valid_request_panics() {
        for seed in SEEDS {
            for cut in 0..seed.len() {
                let _ = parse_line(&seed[..cut]);
            }
        }
    }

    /// One byte replaced, everywhere, with the characters that carry meaning in
    /// JSON. This is where a hand-written parser would fall over.
    #[test]
    fn no_single_byte_corruption_panics() {
        for seed in SEEDS {
            for at in 0..seed.len() {
                for byte in *br#""{}[]:,\0"# {
                    let mut bytes = seed.as_bytes().to_vec();
                    bytes[at] = byte;
                    if let Ok(line) = std::str::from_utf8(&bytes) {
                        let _ = parse_line(line);
                    }
                }
            }
        }
    }

    /// The typed boundary itself, with every method against every shape of
    /// params — including the ones that belong to a different method.
    #[test]
    fn no_method_and_params_combination_panics() {
        let shapes = [
            serde_json::json!(null),
            serde_json::json!({}),
            serde_json::json!([]),
            serde_json::json!(0),
            serde_json::json!("line"),
            serde_json::json!({"line": 7}),
            serde_json::json!({"protocol": []}),
            serde_json::json!({"line": {"nested": {"deeply": true}}}),
        ];
        let methods = ["", "hello", "cmd.exec_line", "time.now", "nope", "hello\0"];
        for method in methods {
            for shape in &shapes {
                let _ = Request::of(method, Some(shape.clone()));
                let _ = Request::of(method, None);
            }
        }
    }

    /// Random bytes, as the belt to the braces above.
    #[test]
    fn no_random_line_panics() {
        let mut state = 0x2545_F491_4F6C_DD1D;
        for _ in 0..2_000 {
            let len = (xorshift(&mut state) % 64) as usize;
            let line: String = (0..len)
                .map(|_| {
                    // Weighted towards structural characters; uniform bytes are
                    // rejected by the first `serde_json` call every time.
                    const ALPHABET: &[u8] = br#"{}[]":,0123456789abcdefghijklmnop \n\t"#;
                    ALPHABET[(xorshift(&mut state) as usize) % ALPHABET.len()] as char
                })
                .collect();
            let _ = parse_line(&line);
        }
    }
}
