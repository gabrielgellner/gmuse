//! **One connected view** — #216, §7.8, §7.10, I1.
//!
//! A reader, a bounded [`Outbox`], and a writer thread. The engine pushes and
//! returns; only the writer ever touches the socket.
//!
//! ## I1 is a property of the shape, not a rule to remember
//!
//! "The engine never blocks on a view" is easy to state and easy to violate by
//! accident — one `writeln!` on the wrong thread does it. Here there is no
//! other thread that *can*: publishing is `Outbox::push_event`, which takes a
//! lock for the length of a `VecDeque` operation and does no I/O at all. A slow
//! client is felt as a growing queue, and then as coalescing, and then as a
//! resync notice. It is never felt as a stall.
//!
//! ## Why requests are dispatched rather than answered in order
//!
//! §7.2 says responses have no ordering guarantee, and calls it a requirement
//! rather than a permission. A reader that answered each line before reading
//! the next would make a cue press wait behind a scan — the head-of-line
//! blocking I6 exists to forbid. So the reader parses and hands off, and the
//! writer sends answers in the order they are finished.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use gmuse_proto::{Event, Response, Topic};

use super::outbox::Outbox;
pub use super::outbox::Outgoing;

/// How far behind one client may fall before its events are discarded.
///
/// **Sized for a view that stopped reading, not for one that is merely busy.**
/// A redraw coalesces to one `playback` event, so a client keeping up at all
/// sits at a handful; reaching sixty-four distinct pending messages means it is
/// not reading, and telling it to re-query is both cheaper and more accurate
/// than continuing to buffer on its behalf.
pub const DEFAULT_CAPACITY: usize = 64;

/// What one connection is subscribed to, and what is queued for it.
pub struct Client {
    outbox: Mutex<Outbox>,
    /// Woken when something is queued, so the writer does not poll.
    ready: Condvar,
    topics: Mutex<Vec<Topic>>,
    gone: AtomicBool,
    /// **Something to hang up with** — #251, §7.10.
    ///
    /// A socket handle, so the engine can end a connection rather than only stop
    /// queueing for it. `close()` alone cannot: it releases a writer waiting on the
    /// condvar, but a writer already inside `write` is parked in the kernel with a full
    /// send buffer, and the reader is parked in `read`. Shutting the socket down returns
    /// both of them with an error, which is a path they already handle.
    ///
    /// `None` in tests that need no socket, which is most of them.
    hangup: Mutex<Option<std::os::unix::net::UnixStream>>,
}

impl Client {
    pub fn new(capacity: usize) -> Self {
        Self {
            outbox: Mutex::new(Outbox::new(capacity)),
            ready: Condvar::new(),
            // **Nothing until asked** — §7.8 is subscription-based, so a client
            // that never subscribes is never sent an event it did not want.
            topics: Mutex::new(Vec::new()),
            gone: AtomicBool::new(false),
            hangup: Mutex::new(None),
        }
    }

    /// Give this client a socket to be hung up with — #251.
    ///
    /// Separate from `new` because the connection is built before its socket is cloned,
    /// and because every test that does not have one would otherwise have to invent one.
    pub fn on_socket(&self, stream: std::os::unix::net::UnixStream) {
        *self.hangup.lock().expect("hangup") = Some(stream);
    }

    /// **End the connection, because this client is not reading** — #251, §7.10.
    ///
    /// §7.10 says a client that stays over the limit MAY be disconnected, and until now
    /// nothing did: `over_limit` measured it and the comment handed the decision to
    /// "whoever owns the socket", who was nobody.
    ///
    /// The reason it has to be a shutdown and not a `close` is [`Self::hangup`]: both of
    /// this connection's threads are parked in the kernel, not on our condvar.
    fn hang_up(&self) {
        if let Some(socket) = self.hangup.lock().expect("hangup").as_ref() {
            let _ = socket.shutdown(std::net::Shutdown::Both);
        }
        self.close();
    }

    pub fn subscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        let mut held = self.topics.lock().expect("topics");
        for topic in topics {
            if !held.contains(topic) {
                held.push(*topic);
            }
        }
        held.sort();
        held.clone()
    }

    pub fn unsubscribe(&self, topics: &[Topic]) -> Vec<Topic> {
        let mut held = self.topics.lock().expect("topics");
        held.retain(|t| !topics.contains(t));
        held.clone()
    }

    pub fn topics(&self) -> Vec<Topic> {
        self.topics.lock().expect("topics").clone()
    }

    /// Queue an answer. Never dropped, and never blocking.
    ///
    /// **This is the path §7.10's limit actually guards** (#251). Events are bounded by
    /// the overflow — they are discarded and a resync notice replaces them — but
    /// `push_response` has no bound at all, and deliberately: *"the bound is not their
    /// fault and somebody is waiting on each."* So a client that asks and never reads
    /// makes the engine hold every answer, without limit.
    ///
    /// Exceeding the capacity in *responses* therefore means something specific and
    /// pathological: more unanswered questions outstanding than the whole outbox holds.
    /// That is the condition to hang up on, and it is not the same as "slow".
    pub fn respond(&self, response: Response) {
        let over = {
            let mut outbox = self.outbox.lock().expect("outbox");
            outbox.push_response(response);
            outbox.over_limit()
        };
        self.ready.notify_one();
        // **After the lock, not inside it.** `hang_up` calls `close`, which takes the
        // outbox lock — doing this while still holding it is a deadlock, and one that
        // would only appear under exactly the load this exists to handle.
        if over {
            self.hang_up();
        }
    }

    /// Offer an event. Ignored unless this client asked for the topic.
    ///
    /// Returns whether it was queued, which is not the same as whether it will
    /// be sent: a queued event may still be coalesced away by a newer one.
    pub fn publish(&self, event: &Event) -> bool {
        if !self.topics.lock().expect("topics").contains(&event.topic) {
            return false;
        }
        let queued = {
            let mut outbox = self.outbox.lock().expect("outbox");
            outbox.push_event(event.clone())
        };
        self.ready.notify_one();
        queued
    }

    /// Wait for something to send. `None` once the client is gone.
    pub fn next(&self) -> Option<Outgoing> {
        let mut outbox = self.outbox.lock().expect("outbox");
        loop {
            if self.gone.load(Ordering::Acquire) {
                return None;
            }
            if let Some(item) = outbox.pop() {
                return Some(item);
            }
            outbox = self.ready.wait(outbox).expect("outbox");
        }
    }

    /// How many events this client has been too slow to receive.
    pub fn dropped(&self) -> u64 {
        self.outbox.lock().expect("outbox").dropped()
    }

    pub fn queued(&self) -> usize {
        self.outbox.lock().expect("outbox").len()
    }

    /// **Disconnect is a non-event for the engine** — §7.10. Its subscriptions
    /// go and nothing else changes.
    pub fn close(&self) {
        self.gone.store(true, Ordering::Release);
        self.outbox.lock().expect("outbox").close();
        self.topics.lock().expect("topics").clear();
        self.ready.notify_all();
    }

    pub fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }
}

/// Everyone currently connected.
#[derive(Default)]
pub struct Subscribers {
    clients: Mutex<Vec<Arc<Client>>>,
}

impl Subscribers {
    pub fn add(&self, client: Arc<Client>) {
        self.clients.lock().expect("clients").push(client);
    }

    /// Offer an event to everyone subscribed to its topic.
    ///
    /// **Nothing here can block on a socket**, which is the whole of I1: this
    /// walks a vector, takes a short lock per client and returns. A client that
    /// has gone is forgotten on the way past.
    pub fn publish(&self, event: &Event) -> usize {
        let mut clients = self.clients.lock().expect("clients");
        clients.retain(|c| !c.is_gone());
        clients.iter().filter(|c| c.publish(event)).count()
    }

    pub fn len(&self) -> usize {
        self.clients.lock().expect("clients").len()
    }

    /// How much is still waiting to be written, across every client.
    pub fn pending(&self) -> usize {
        self.clients
            .lock()
            .expect("clients")
            .iter()
            .map(|c| c.queued())
            .sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(topic: Topic) -> Event {
        Event::new(topic, serde_json::json!({"n":1}))
    }

    #[test]
    fn a_client_receives_nothing_until_it_subscribes() {
        let client = Client::new(8);
        assert!(!client.publish(&event(Topic::Queue)), "not asked for");
        client.subscribe(&[Topic::Queue]);
        assert!(client.publish(&event(Topic::Queue)));
        assert!(!client.publish(&event(Topic::Meters)), "a different topic");
    }

    #[test]
    fn unsubscribing_stops_delivery_without_touching_the_rest() {
        let client = Client::new(8);
        client.subscribe(&[Topic::Queue, Topic::Library]);
        assert_eq!(client.unsubscribe(&[Topic::Queue]), vec![Topic::Library]);
        assert!(!client.publish(&event(Topic::Queue)));
        assert!(client.publish(&event(Topic::Library)));
    }

    #[test]
    fn subscribing_twice_does_not_duplicate_a_topic() {
        let client = Client::new(8);
        client.subscribe(&[Topic::Queue]);
        assert_eq!(client.subscribe(&[Topic::Queue]), vec![Topic::Queue]);
    }

    /// §7.10: a gone client is forgotten, and publishing is not an error.
    #[test]
    fn a_departed_client_is_dropped_from_the_subscribers() {
        let subscribers = Subscribers::default();
        let alive = Arc::new(Client::new(8));
        let gone = Arc::new(Client::new(8));
        alive.subscribe(&[Topic::Queue]);
        gone.subscribe(&[Topic::Queue]);
        subscribers.add(Arc::clone(&alive));
        subscribers.add(Arc::clone(&gone));

        gone.close();
        assert_eq!(subscribers.publish(&event(Topic::Queue)), 1);
        assert_eq!(subscribers.len(), 1, "and it is not walked again");
    }

    /// I1, as directly as it can be asserted without a socket: publishing to a
    /// client nobody is reading returns rather than waiting.
    #[test]
    fn publishing_to_a_client_that_never_reads_does_not_block() {
        let client = Client::new(2);
        client.subscribe(Topic::ALL);
        for n in 0..1_000 {
            client.publish(&Event::keyed(
                Topic::Params,
                n.to_string(),
                serde_json::json!({ "n": n }),
            ));
        }
        assert!(client.dropped() > 0, "it fell behind");
        assert!(client.queued() <= 3, "and was not allowed to grow");
    }

    #[test]
    fn a_waiting_writer_is_woken_by_a_response() {
        let client = Arc::new(Client::new(8));
        let writer = {
            let client = Arc::clone(&client);
            std::thread::spawn(move || client.next())
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        client.respond(Response::ok(None, serde_json::json!("hi")));
        let got = writer.join().expect("writer");
        assert!(matches!(got, Some(Outgoing::Response(_))));
    }

    #[test]
    fn closing_releases_a_waiting_writer() {
        let client = Arc::new(Client::new(8));
        let writer = {
            let client = Arc::clone(&client);
            std::thread::spawn(move || client.next())
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        client.close();
        assert!(writer.join().expect("writer").is_none());
    }
}
