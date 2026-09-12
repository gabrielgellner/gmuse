//! **What the engine says without being asked** — §7.8, §7.10.
//!
//! ## Events describe state, not edits
//!
//! This is the sentence the rest of the design hangs off. An event says *what
//! is true now*, not *what changed since the last one*, so a client that missed
//! three of them is behind rather than wrong — it can always recover by
//! re-querying.
//!
//! That is what licenses coalescing: replacing a pending `playback` event with
//! a newer one loses nothing, because the newer one is a complete statement.
//! Under a replay model the same act would corrupt the client's state, and
//! §7.10's whole backpressure story would be unavailable.

use serde::{Deserialize, Serialize};

/// What a client can subscribe to — §7.8.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Topic {
    /// Transport state, on change only. Never a steady position stream: a
    /// client extrapolates locally from `pos_frames` and `at_ns` (§7.8).
    Playback,
    /// `{version}`, optionally with a small diff.
    Queue,
    /// `{version, scope, ids?}`, shaped by the fact's blast radius (§8.7).
    Library,
    /// Load state, analysis readiness, sync master.
    Decks,
    /// Changed parameter values. Coalesced, at most about 30 Hz.
    Params,
    /// Peak and RMS per bus. Optional, at most about 30 Hz.
    Meters,
    /// Status, errors, xruns, and `resync_required`.
    Engine,
}

impl Topic {
    /// Every topic, for `sub.subscribe` with no argument and for tests that
    /// must not quietly stop covering one.
    pub const ALL: &'static [Topic] = &[
        Topic::Playback,
        Topic::Queue,
        Topic::Library,
        Topic::Decks,
        Topic::Params,
        Topic::Meters,
        Topic::Engine,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Topic::Playback => "playback",
            Topic::Queue => "queue",
            Topic::Library => "library",
            Topic::Decks => "decks",
            Topic::Params => "params",
            Topic::Meters => "meters",
            Topic::Engine => "engine",
        }
    }
}

/// One thing the engine has to say.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    pub topic: Topic,
    /// **What distinguishes two events on one topic** — §7.10's "per deck or
    /// param where relevant".
    ///
    /// Coalescing is latest-wins *per key*, so a deck A update must not replace
    /// a deck B one: they are different facts, not two versions of one. `None`
    /// means the topic has a single subject and any newer event supersedes the
    /// pending one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// The topic's payload, merged into the notification body.
    #[serde(flatten)]
    pub body: serde_json::Value,
}

impl Event {
    pub fn new(topic: Topic, body: serde_json::Value) -> Self {
        Self {
            topic,
            key: None,
            body,
        }
    }

    /// An event that only supersedes others about the same subject.
    pub fn keyed(topic: Topic, key: impl Into<String>, body: serde_json::Value) -> Self {
        Self {
            topic,
            key: Some(key.into()),
            body,
        }
    }

    /// **The engine dropped events and the client must re-query** — §7.10.
    ///
    /// Not an error: nothing is wrong and nothing is lost that a query cannot
    /// recover. It is the one message that *must* survive, because it is what
    /// tells a client its picture is stale — which is why the overflow path
    /// clears everything else and then queues this.
    pub fn resync_required(reason: &str) -> Self {
        Self::keyed(
            Topic::Engine,
            "resync",
            serde_json::json!({ "resync_required": true, "reason": reason }),
        )
    }

    /// What this event coalesces against. Two events with equal keys are two
    /// versions of one fact; the newer wins.
    pub fn coalescing_key(&self) -> (Topic, Option<&str>) {
        (self.topic, self.key.as_deref())
    }
}

/// The method name every event notification carries — §7.2.
pub const EVENT_METHOD: &str = "event";

/// Frame an event as the JSON-RPC notification a client sees.
///
/// **A notification, so it carries no `id`** and a client never tries to
/// correlate one with a request it sent.
pub fn notification(event: &Event) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": crate::envelope::JSONRPC,
        "method": EVENT_METHOD,
        "params": event,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_topic_round_trips_through_its_wire_name() {
        for topic in Topic::ALL {
            let json = serde_json::to_string(topic).unwrap();
            assert_eq!(json, format!("\"{}\"", topic.as_str()));
            assert_eq!(serde_json::from_str::<Topic>(&json).unwrap(), *topic);
        }
    }

    /// The payload is merged in rather than nested under `body`, so the wire
    /// matches §7.8's example: `{"topic":"playback","state":"playing",…}`.
    #[test]
    fn the_payload_is_flattened_beside_the_topic() {
        let event = Event::new(
            Topic::Playback,
            serde_json::json!({"state":"playing","pos_frames":1234}),
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["topic"], "playback");
        assert_eq!(value["state"], "playing");
        assert_eq!(value["pos_frames"], 1234);
        assert!(value.get("body").is_none(), "not nested: {value}");
    }

    /// **Two decks are two facts, not two versions of one.** The key is what
    /// stops a deck B update swallowing deck A's.
    #[test]
    fn events_about_different_subjects_do_not_share_a_key() {
        let a = Event::keyed(Topic::Decks, "A", serde_json::json!({"loaded":true}));
        let b = Event::keyed(Topic::Decks, "B", serde_json::json!({"loaded":false}));
        assert_ne!(a.coalescing_key(), b.coalescing_key());
    }

    #[test]
    fn events_about_one_subject_share_a_key_and_so_coalesce() {
        let first = Event::new(Topic::Queue, serde_json::json!({"version":1}));
        let later = Event::new(Topic::Queue, serde_json::json!({"version":2}));
        assert_eq!(first.coalescing_key(), later.coalescing_key());
    }

    #[test]
    fn an_event_is_framed_as_a_notification_with_no_id() {
        let value = notification(&Event::new(Topic::Engine, serde_json::json!({"ok":true})));
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["method"], "event");
        assert!(value.get("id").is_none(), "an event is never correlated");
        assert_eq!(value["params"]["topic"], "engine");
    }
}
