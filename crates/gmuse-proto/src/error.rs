//! What the engine says when it will not answer — §7.13.

use serde::{Deserialize, Serialize};

/// A JSON-RPC error object.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

/// The codes of §7.13.
///
/// **The negative ones are JSON-RPC 2.0's and mean what it says they mean**;
/// the four-digit ones are this protocol's own. Kept as constants rather than
/// an enum with a `From`, because a code arriving from a future engine must be
/// reportable by a client built today — an enum would have to grow an `Other`
/// arm that every match then has to handle.
pub mod code {
    /// The line was not JSON.
    pub const PARSE: i32 = -32700;
    /// It was JSON, and not a request this protocol permits — including a
    /// top-level array, which JSON-RPC 2.0 allows and §7.1 does not.
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    /// Data: `{field, message}`.
    pub const INVALID_PARAMS: i32 = -32602;
    pub const INTERNAL: i32 = -32603;

    /// The caller's version of a versioned collection is behind. Data:
    /// `{current}`.
    pub const STALE_VERSION: i32 = 1001;
    /// A reference from a previous generation. Data: `{expected_generation}`.
    pub const STALE_REFERENCE: i32 = 1002;
    /// Data: `{ref}`.
    pub const NOT_FOUND: i32 = 1003;
    /// Data: `{message, span}`.
    pub const QUERY_PARSE: i32 = 1004;
    /// Data: `{message, span}`.
    pub const COMMAND_PARSE: i32 = 1005;
    /// The RT side's scheduled-event capacity is full.
    pub const SCHEDULE_FULL: i32 = 1006;
    /// Data: `{reason}`.
    pub const NOT_READY: i32 = 1007;
    /// Data: `{capability}`.
    pub const UNSUPPORTED: i32 = 1008;
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    pub fn with_data(mut self, data: serde_json::Value) -> Self {
        self.data = Some(data);
        self
    }

    pub fn parse(message: impl Into<String>) -> Self {
        Self::new(code::PARSE, message)
    }

    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_REQUEST, message)
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(code::METHOD_NOT_FOUND, format!("no such method: {method}"))
    }

    /// Data is `{field, message}`, so a client can point at the argument.
    pub fn invalid_params(field: &str, message: impl Into<String>) -> Self {
        let message = message.into();
        Self::new(code::INVALID_PARAMS, message.clone())
            .with_data(serde_json::json!({ "field": field, "message": message }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_error_without_data_omits_the_field_rather_than_sending_null() {
        let line = serde_json::to_string(&RpcError::new(code::INTERNAL, "boom")).unwrap();
        assert_eq!(line, r#"{"code":-32603,"message":"boom"}"#);
    }

    #[test]
    fn invalid_params_says_which_field() {
        let e = RpcError::invalid_params("protocol", "expected gmuse/N");
        assert_eq!(e.code, code::INVALID_PARAMS);
        assert_eq!(e.data.unwrap()["field"], "protocol");
    }
}
