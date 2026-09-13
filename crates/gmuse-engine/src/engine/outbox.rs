//! **What is waiting to go to one client** — #216, §7.10, I1.
//!
//! A bounded queue that coalesces events latest-wins per key and never drops a
//! response.
//!
//! ## Why coalescing is safe, and would not be under a different design
//!
//! §7.8: events describe *state*, not a sequence of edits to replay. A newer
//! `playback` event is a complete statement about the transport, so replacing a
//! pending one with it loses nothing. Under a replay model the same act would
//! corrupt the client's picture and none of this would be available.
//!
//! ## Why responses are different
//!
//! A response is the answer to a question somebody asked, and a client is
//! blocked on its `id`. Dropping one is not "being behind", it is never
//! answering — so the bound is enforced by discarding events, and a client that
//! is still over it after that is a client to disconnect rather than one to
//! silently fail.
//!
//! ## Where I1 lives
//!
//! Nothing here does I/O. The engine pushes and returns; a writer thread pops
//! and writes. That is what makes "the engine never blocks on a view" a
//! property of the structure rather than a rule someone has to remember.

use std::collections::VecDeque;

use gmuse_proto::{Event, Response};

/// One thing to send.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// An answer somebody is waiting on. Never dropped.
    Response(Box<Response>),
    /// A statement about state. Coalesced, and droppable under pressure.
    Event(Event),
}

/// A per-client outbound queue.
#[derive(Debug)]
pub struct Outbox {
    queue: VecDeque<Outgoing>,
    capacity: usize,
    /// Events discarded since the client last caught up — reported, because a
    /// silent drop is indistinguishable from an engine that had nothing to say.
    dropped: u64,
    /// Set when overflow has already queued a resync, so a client that stays
    /// over the limit gets one notice rather than a stream of them.
    resyncing: bool,
    closed: bool,
}

impl Outbox {
    /// **The capacity is a count of pending messages, not bytes.** What matters
    /// is how far behind a client is allowed to fall, and one message is one
    /// unit of behind-ness whatever it weighs.
    pub fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::new(),
            capacity: capacity.max(1),
            dropped: 0,
            resyncing: false,
            closed: false,
        }
    }

    pub fn len(&self) -> usize {
        self.queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }

    pub fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Whether the client has been told to re-query and has not caught up.
    pub fn resyncing(&self) -> bool {
        self.resyncing
    }

    /// **A response is always queued** — §7.10. Somebody is blocked on it.
    pub fn push_response(&mut self, response: Response) {
        if self.closed {
            return;
        }
        self.queue.push_back(Outgoing::Response(Box::new(response)));
    }

    /// Queue an event, coalescing it against a pending one about the same
    /// subject.
    ///
    /// Returns `false` when the event was dropped into an overflow rather than
    /// queued, which the caller may want to count.
    pub fn push_event(&mut self, event: Event) -> bool {
        if self.closed {
            return false;
        }
        // **Latest-wins, in place.** Replacing where the old one sat rather
        // than moving it to the back keeps an event from starving behind a
        // stream of its own updates — a `library` event that kept being
        // superseded would otherwise never be sent while anything was changing.
        let key = event.coalescing_key();
        if let Some(slot) = self.queue.iter_mut().find(|pending| match pending {
            Outgoing::Event(pending) => pending.coalescing_key() == key,
            Outgoing::Response(_) => false,
        }) {
            // **Not simply "the newer one wins"** — §8.7. `Event::coalesce`
            // decides, because what two pending events mean together is a fact
            // about the topic's payload rather than about this queue: a
            // `playback` event is a complete statement and supersedes, a
            // `library` event is a list of what changed and must be unioned or
            // the rows in the older list stay stale on screen for ever.
            if let Outgoing::Event(pending) = slot {
                *slot = Outgoing::Event(pending.coalesce(&event));
            }
            return true;
        }

        if self.queue.len() < self.capacity {
            self.queue.push_back(Outgoing::Event(event));
            return true;
        }

        self.overflow("the outbound queue filled");
        false
    }

    /// **Discard every pending event and say so** — §7.10.
    ///
    /// Responses survive, because the bound is not their fault and somebody is
    /// waiting on each. The resync notice goes *after* them so that a client
    /// reading in order answers its outstanding questions before it is told to
    /// ask new ones.
    fn overflow(&mut self, reason: &str) {
        let before = self.queue.len();
        self.queue
            .retain(|item| matches!(item, Outgoing::Response(_)));
        // The event that triggered this is dropped too, and never reached the
        // queue to be counted by the retain.
        self.dropped += (before - self.queue.len()) as u64 + 1;

        // **The notice is re-queued every time, and that is not a duplicate.**
        //
        // The first version guarded this with `resyncing`, so a client that
        // stayed behind got one notice — which was the intent and not what
        // happened. The retain above discards the *previous* notice along with
        // everything else, so the guard meant the second overflow cleared the
        // queue and put nothing back: a client under sustained pressure ended
        // up with an empty outbox and no idea it was stale. Two tests caught
        // it, which is the only reason this comment is not a bug report.
        //
        // Exactly one notice is pending at any time because the clear
        // immediately precedes the push, so re-queuing cannot accumulate them.
        self.queue
            .push_back(Outgoing::Event(Event::resync_required(reason)));
        self.resyncing = true;
    }

    /// Take the next thing to send.
    pub fn pop(&mut self) -> Option<Outgoing> {
        let next = self.queue.pop_front();
        // Caught up: the next overflow is a new event worth reporting.
        if self.queue.is_empty() {
            self.resyncing = false;
        }
        next
    }

    /// Whether this client has stayed over the limit — §7.10's "MAY be
    /// disconnected". Reported rather than acted on here, because the decision
    /// belongs to whoever owns the socket.
    pub fn over_limit(&self) -> bool {
        self.queue.len() > self.capacity
    }

    /// Stop accepting. Used when the connection has gone.
    pub fn close(&mut self) {
        self.closed = true;
        self.queue.clear();
    }

    pub fn is_closed(&self) -> bool {
        self.closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmuse_proto::{Id, Topic};

    fn response(id: i64) -> Response {
        Response::ok(Some(Id::Number(id)), serde_json::json!(id))
    }

    fn playback(pos: i64) -> Event {
        Event::new(Topic::Playback, serde_json::json!({ "pos_frames": pos }))
    }

    fn events(outbox: &mut Outbox) -> Vec<Event> {
        let mut out = Vec::new();
        while let Some(item) = outbox.pop() {
            if let Outgoing::Event(e) = item {
                out.push(e);
            }
        }
        out
    }

    #[test]
    fn an_event_about_one_subject_replaces_the_pending_one() {
        let mut outbox = Outbox::new(8);
        for pos in [1, 2, 3] {
            outbox.push_event(playback(pos));
        }
        assert_eq!(outbox.len(), 1, "three statements about one thing is one");
        let sent = events(&mut outbox);
        assert_eq!(sent[0].body["pos_frames"], 3, "and it is the newest");
    }

    /// **Replaced in place, not moved to the back.** Otherwise an event that
    /// keeps being superseded never reaches a client while anything is
    /// changing — the queue would always have something newer ahead of it.
    #[test]
    fn coalescing_keeps_the_position_the_first_one_had() {
        let mut outbox = Outbox::new(8);
        outbox.push_event(Event::new(Topic::Library, serde_json::json!({"v":1})));
        outbox.push_event(Event::new(Topic::Queue, serde_json::json!({"v":1})));
        outbox.push_event(Event::new(Topic::Library, serde_json::json!({"v":2})));

        let sent = events(&mut outbox);
        assert_eq!(sent[0].topic, Topic::Library, "it kept its place");
        assert_eq!(sent[0].body["v"], 2, "with the newer value");
        assert_eq!(sent[1].topic, Topic::Queue);
    }

    /// The reason `push_event` asks the event rather than assuming: a library
    /// change is a list, and replacing one loses rows nothing will mention
    /// again.
    #[test]
    fn pending_library_changes_are_unioned_not_replaced() {
        let mut outbox = Outbox::new(8);
        outbox.push_event(Event::new(
            Topic::Library,
            serde_json::json!({"version":1,"scope":"ids","ids":["rec:a"]}),
        ));
        outbox.push_event(Event::new(
            Topic::Library,
            serde_json::json!({"version":2,"scope":"ids","ids":["rec:b"]}),
        ));
        assert_eq!(outbox.len(), 1);
        let sent = events(&mut outbox);
        assert_eq!(sent[0].body["ids"], serde_json::json!(["rec:a", "rec:b"]));
    }

    #[test]
    fn events_about_different_subjects_do_not_swallow_each_other() {
        let mut outbox = Outbox::new(8);
        outbox.push_event(Event::keyed(Topic::Decks, "A", serde_json::json!({"n":1})));
        outbox.push_event(Event::keyed(Topic::Decks, "B", serde_json::json!({"n":2})));
        assert_eq!(outbox.len(), 2, "two decks are two facts");
    }

    /// §7.10, and the reason the bound is not simply "drop the oldest": a
    /// response is the answer to a question a client is blocked on.
    #[test]
    fn responses_are_never_dropped_however_full_it_gets() {
        let mut outbox = Outbox::new(2);
        outbox.push_response(response(1));
        outbox.push_response(response(2));
        outbox.push_response(response(3));
        outbox.push_event(playback(1));

        let mut ids = Vec::new();
        while let Some(item) = outbox.pop() {
            if let Outgoing::Response(r) = item {
                ids.push(r.id.clone().unwrap());
            }
        }
        assert_eq!(
            ids,
            vec![Id::Number(1), Id::Number(2), Id::Number(3)],
            "every answer survived"
        );
    }

    #[test]
    fn overflow_discards_events_and_queues_one_resync() {
        let mut outbox = Outbox::new(2);
        // Distinct keys, so nothing coalesces and the bound is actually reached.
        outbox.push_event(Event::keyed(Topic::Params, "a", serde_json::json!({})));
        outbox.push_event(Event::keyed(Topic::Params, "b", serde_json::json!({})));
        assert!(!outbox.push_event(Event::keyed(Topic::Params, "c", serde_json::json!({}))));

        let sent = events(&mut outbox);
        assert_eq!(sent.len(), 1, "everything pending went");
        assert_eq!(sent[0].topic, Topic::Engine);
        assert_eq!(sent[0].body["resync_required"], true);
        assert_eq!(
            outbox.dropped(),
            3,
            "the two pending, and the one that would not fit"
        );
    }

    /// A client that stays behind gets told once, not once per event — a
    /// stream of resync notices is itself the pressure it is reporting.
    #[test]
    fn a_client_that_stays_behind_is_told_once() {
        let mut outbox = Outbox::new(2);
        for k in ["a", "b", "c", "d", "e"] {
            outbox.push_event(Event::keyed(Topic::Params, k, serde_json::json!({})));
        }
        let sent = events(&mut outbox);
        assert_eq!(sent.len(), 1, "one notice, not four: {sent:?}");
    }

    /// Once it has caught up the next overflow is news again.
    #[test]
    fn catching_up_rearms_the_notice() {
        let mut outbox = Outbox::new(1);
        outbox.push_event(Event::keyed(Topic::Params, "a", serde_json::json!({})));
        outbox.push_event(Event::keyed(Topic::Params, "b", serde_json::json!({})));
        assert!(outbox.resyncing());
        while outbox.pop().is_some() {}
        assert!(!outbox.resyncing(), "drained means caught up");

        outbox.push_event(Event::keyed(Topic::Params, "c", serde_json::json!({})));
        outbox.push_event(Event::keyed(Topic::Params, "d", serde_json::json!({})));
        let sent = events(&mut outbox);
        assert!(
            sent.iter().any(|e| e.body["resync_required"] == true),
            "a second period of pressure is reported"
        );
    }

    /// The resync goes after pending responses, so a client reading in order
    /// answers what it already asked before being told to ask again.
    #[test]
    fn the_resync_follows_the_answers_already_owed() {
        let mut outbox = Outbox::new(1);
        outbox.push_response(response(1));
        outbox.push_event(Event::keyed(Topic::Params, "a", serde_json::json!({})));
        outbox.push_event(Event::keyed(Topic::Params, "b", serde_json::json!({})));

        assert!(matches!(outbox.pop(), Some(Outgoing::Response(_))));
        let Some(Outgoing::Event(e)) = outbox.pop() else {
            panic!("the resync follows it")
        };
        assert_eq!(e.body["resync_required"], true);
    }

    #[test]
    fn a_closed_outbox_accepts_nothing_further() {
        let mut outbox = Outbox::new(4);
        outbox.push_event(playback(1));
        outbox.close();
        outbox.push_response(response(1));
        assert!(!outbox.push_event(playback(2)));
        assert!(outbox.is_empty(), "a gone client is not a backlog");
    }
}
