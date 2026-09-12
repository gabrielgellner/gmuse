//! The JSON-RPC 2.0 envelope, as §7.2 narrows it.

use serde::{Deserialize, Serialize};

use crate::error::{RpcError, code};

/// The `"2.0"` every message carries.
pub const JSONRPC: &str = "2.0";

/// A request or a notification, which differ only by whether `id` is there.
///
/// **One struct rather than two**, because the wire does not distinguish them
/// at the framing level and a client may legitimately send either: a request
/// wants an answer, a notification is fire-and-forget (§7.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub jsonrpc: String,
    /// Absent for a notification, which gets no reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Id>,
    pub method: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

/// A request id, which JSON-RPC allows to be a number or a string.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Id {
    Number(i64),
    Text(String),
}

impl Request {
    /// Whether this wants a reply.
    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }

    /// Check what the envelope itself promises, before anything reads `method`.
    pub fn validate(&self) -> Result<(), RpcError> {
        if self.jsonrpc != JSONRPC {
            return Err(RpcError::invalid_request(format!(
                "jsonrpc must be \"{JSONRPC}\", got {:?}",
                self.jsonrpc
            )));
        }
        if self.method.is_empty() {
            return Err(RpcError::invalid_request("method must not be empty"));
        }
        Ok(())
    }
}

/// Exactly one of a result or an error, carrying the id it answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    /// Null when the request could not be parsed far enough to have one.
    pub id: Option<Id>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(id: Option<Id>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn err(id: Option<Id>, error: RpcError) -> Self {
        Self {
            jsonrpc: JSONRPC.into(),
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// The longest line the engine will read — §7.1.
///
/// A megabyte is far past any legitimate request and well short of letting a
/// stuck or hostile writer grow the engine's memory without bound.
pub const MAX_LINE: usize = 1024 * 1024;

/// Turn one received line into a request, or into the error to send back.
///
/// **A top-level array is refused** (§7.1). JSON-RPC 2.0 permits batching and
/// this protocol does not, which is said here rather than only in prose: a
/// server that silently ignored a batch would look like one that supports it
/// and lost the messages. Checked before parsing, so the refusal does not
/// depend on the batch's contents being valid.
pub fn parse_line(line: &str) -> Result<Request, RpcError> {
    if line.len() > MAX_LINE {
        return Err(RpcError::invalid_request(format!(
            "line of {} bytes exceeds the {MAX_LINE}-byte maximum",
            line.len()
        )));
    }
    if line.trim_start().starts_with('[') {
        return Err(RpcError::new(
            code::INVALID_REQUEST,
            "batches are not supported: send one object per line",
        ));
    }
    let request: Request =
        serde_json::from_str(line).map_err(|e| RpcError::parse(format!("not a request: {e}")))?;
    request.validate()?;
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(line: &str) -> Result<Request, RpcError> {
        parse_line(line)
    }

    #[test]
    fn a_request_carries_an_id_and_a_notification_does_not() {
        let r = req(r#"{"jsonrpc":"2.0","id":1,"method":"hello"}"#).unwrap();
        assert!(!r.is_notification());
        assert_eq!(r.id, Some(Id::Number(1)));

        let n = req(r#"{"jsonrpc":"2.0","method":"hello"}"#).unwrap();
        assert!(n.is_notification());
    }

    /// JSON-RPC allows a string id and clients use them; correlating by id is
    /// the client's job (§7.2) and it must get back what it sent.
    #[test]
    fn a_string_id_survives_the_round_trip() {
        let r = req(r#"{"jsonrpc":"2.0","id":"abc","method":"hello"}"#).unwrap();
        assert_eq!(r.id, Some(Id::Text("abc".into())));
        let back = serde_json::to_string(&Response::ok(r.id, serde_json::json!({}))).unwrap();
        assert!(back.contains(r#""id":"abc""#), "{back}");
    }

    /// **Not "unsupported method"** — the array never gets far enough to have
    /// one. §7.1 says a line beginning with `[` is a -32600.
    #[test]
    fn a_batch_is_refused_as_an_invalid_request() {
        let e = req(r#"[{"jsonrpc":"2.0","id":1,"method":"hello"}]"#).unwrap_err();
        assert_eq!(e.code, code::INVALID_REQUEST);
        assert!(e.message.contains("batches"), "{}", e.message);
    }

    /// Refused on its shape, so a batch of nonsense is still a batch error and
    /// not a parse error that happens to mention a bracket.
    #[test]
    fn a_batch_of_rubbish_is_still_a_batch_error() {
        assert_eq!(
            req("[ not json at all").unwrap_err().code,
            code::INVALID_REQUEST
        );
    }

    #[test]
    fn a_line_that_is_not_json_is_a_parse_error() {
        assert_eq!(req("pause").unwrap_err().code, code::PARSE);
    }

    #[test]
    fn the_wrong_jsonrpc_version_is_an_invalid_request() {
        let e = req(r#"{"jsonrpc":"1.0","id":1,"method":"hello"}"#).unwrap_err();
        assert_eq!(e.code, code::INVALID_REQUEST);
    }

    #[test]
    fn an_oversized_line_is_refused_without_being_parsed() {
        let line = format!(
            r#"{{"jsonrpc":"2.0","id":1,"method":"{}"}}"#,
            "x".repeat(MAX_LINE)
        );
        let e = req(&line).unwrap_err();
        assert_eq!(e.code, code::INVALID_REQUEST);
        assert!(e.message.contains("exceeds"), "{}", e.message);
    }

    #[test]
    fn a_response_carries_a_result_or_an_error_and_never_both() {
        let ok = serde_json::to_string(&Response::ok(Some(Id::Number(1)), serde_json::json!(7)))
            .unwrap();
        assert!(
            ok.contains(r#""result":7"#) && !ok.contains("error"),
            "{ok}"
        );

        let bad = serde_json::to_string(&Response::err(
            Some(Id::Number(1)),
            RpcError::method_not_found("nope"),
        ))
        .unwrap();
        assert!(bad.contains("error") && !bad.contains("result"), "{bad}");
    }
}
