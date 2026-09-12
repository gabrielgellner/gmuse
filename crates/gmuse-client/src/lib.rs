//! **The one client every view speaks through** — #217, §4.2, §13.3.
//!
//! Connect, handshake, correlate by `id`, receive events. Blocking, because
//! every caller it has is a thread that wants an answer: a CLI subcommand, the
//! TUI's control thread, an MCP adapter's request handler.
//!
//! ## Correlation is the client's job, and it is not optional
//!
//! §7.2: responses have no ordering guarantee, and events interleave with them.
//! That is a requirement on the engine — a response queue preserving request
//! order would make a cue press wait behind a scan — and its cost lands here.
//! A client that read the next line and assumed it was its answer would work
//! perfectly until the engine got busy, which is the worst way for this to
//! fail.
//!
//! So one reader thread demultiplexes: a line with an `id` goes to whoever is
//! waiting on it, and a line whose method is `event` goes to the event stream.
//! Nothing else can read the socket.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, bounded, unbounded};
use gmuse_proto::hello::{HelloParams, HelloResult};
use gmuse_proto::{Event, Id, RpcError, Topic};

/// What went wrong between here and the engine.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("could not reach the engine at {path}: {source}")]
    Connect {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the connection closed while waiting for a reply")]
    Closed,
    #[error("the engine refused: {0}")]
    Refused(RpcError),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("the engine sent something unreadable: {0}")]
    Malformed(String),
    #[error("timed out after {0:?}")]
    TimedOut(Duration),
}

impl ClientError {
    /// The engine's own error, where there is one — so a caller can match on a
    /// code (§7.13) rather than on a message.
    pub fn rpc(&self) -> Option<&RpcError> {
        match self {
            Self::Refused(e) => Some(e),
            _ => None,
        }
    }
}

type Pending = Arc<Mutex<HashMap<Id, Sender<Result<serde_json::Value, RpcError>>>>>;

/// A connection to a running engine.
pub struct Client {
    write: Mutex<UnixStream>,
    pending: Pending,
    events: Receiver<Event>,
    hello: HelloResult,
    next_id: AtomicI64,
}

impl Client {
    /// Connect and shake hands.
    ///
    /// **The handshake is not optional and not deferred**: §7.3 says the first
    /// message on a connection MUST be `hello`, so doing it here means no
    /// caller can forget and no caller has to remember.
    pub fn connect(path: &Path, name: &str, version: &str) -> Result<Self, ClientError> {
        let stream = UnixStream::connect(path).map_err(|source| ClientError::Connect {
            path: path.display().to_string(),
            source,
        })?;
        let read_half = stream.try_clone()?;

        let pending: Pending = Arc::default();
        let (events_tx, events) = unbounded();

        {
            let pending = Arc::clone(&pending);
            std::thread::Builder::new()
                .name("gmuse-client-reader".into())
                .spawn(move || demultiplex(read_half, &pending, &events_tx))?;
        }

        let client = Self {
            write: Mutex::new(stream),
            pending,
            events,
            hello: HelloResult {
                protocol: String::new(),
                engine_generation: String::new(),
                stream_generation: 0,
                engine_now_ns: 0,
                sample_rate: 0,
                capabilities: Vec::new(),
            },
            next_id: AtomicI64::new(1),
        };

        let params = HelloParams::new(name, version);
        let result = client.call("hello", Some(serde_json::to_value(&params)?))?;
        let hello: HelloResult = serde_json::from_value(result)
            .map_err(|e| ClientError::Malformed(format!("hello result: {e}")))?;

        Ok(Self { hello, ..client })
    }

    /// What the engine said about itself.
    pub fn hello(&self) -> &HelloResult {
        &self.hello
    }

    /// Send a request and wait for the answer that carries its id.
    pub fn call(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<serde_json::Value, ClientError> {
        self.ask(method, params, None)
    }

    /// The same, giving up after `within`.
    ///
    /// **For a caller that must not be held.** `call` waits as long as it takes,
    /// which is right for `gmuse ctl` — a person can interrupt it — and wrong on
    /// a draw thread, where a wedged engine would freeze the screen. The engine
    /// makes the same argument to itself in `Bare::queue`, which bounds its wait
    /// on the control thread for the same reason one layer in.
    pub fn call_within(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        within: Duration,
    ) -> Result<serde_json::Value, ClientError> {
        self.ask(method, params, Some(within))
    }

    fn ask(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        within: Option<Duration>,
    ) -> Result<serde_json::Value, ClientError> {
        let id = Id::Number(self.next_id.fetch_add(1, Ordering::Relaxed));
        let (tx, rx) = bounded(1);
        self.pending.lock().expect("pending").insert(id.clone(), tx);

        let mut body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
        });
        if let Some(params) = params {
            body["params"] = params;
        }

        {
            let mut write = self.write.lock().expect("write");
            if let Err(e) = writeln!(write, "{body}").and_then(|()| write.flush()) {
                self.pending.lock().expect("pending").remove(&id);
                return Err(e.into());
            }
        }

        let heard = match within {
            Some(limit) => rx.recv_timeout(limit).map_err(|e| {
                // **Forget the correlation on the way out.** A timed-out call
                // that left its sender in `pending` would leak an entry per
                // give-up, and the reply — if it ever came — would be delivered
                // to nobody while still looking outstanding.
                self.pending.lock().expect("pending").remove(&id);
                match e {
                    crossbeam_channel::RecvTimeoutError::Timeout => ClientError::TimedOut(limit),
                    crossbeam_channel::RecvTimeoutError::Disconnected => ClientError::Closed,
                }
            }),
            None => rx.recv().map_err(|_| ClientError::Closed),
        };
        match heard {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(e)) => Err(ClientError::Refused(e)),
            // The reader thread dropped the sender, which it does when the
            // socket closes.
            Err(e) => Err(e),
        }
    }

    /// Send a request without waiting — §7.2's notifications.
    pub fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), ClientError> {
        let mut body = serde_json::json!({ "jsonrpc": "2.0", "method": method });
        if let Some(params) = params {
            body["params"] = params;
        }
        let mut write = self.write.lock().expect("write");
        writeln!(write, "{body}")?;
        write.flush()?;
        Ok(())
    }

    /// Subscribe, and get back the topics now in force.
    pub fn subscribe(&self, topics: &[Topic]) -> Result<Vec<Topic>, ClientError> {
        let params = serde_json::json!({ "topics": topics });
        let result = self.call(gmuse_proto::request::method::SUBSCRIBE, Some(params))?;
        serde_json::from_value(result["topics"].clone())
            .map_err(|e| ClientError::Malformed(format!("subscribe result: {e}")))
    }

    /// The events this connection has been sent.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Wait for one event, or give up.
    pub fn next_event(&self, within: Duration) -> Result<Event, ClientError> {
        self.events
            .recv_timeout(within)
            .map_err(|_| ClientError::TimedOut(within))
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(e: serde_json::Error) -> Self {
        Self::Malformed(e.to_string())
    }
}

/// Read every line and send it where it belongs.
///
/// **The only reader.** Everything about correlation lives here, so a caller
/// cannot accidentally read a line that was not its own.
fn demultiplex(stream: UnixStream, pending: &Pending, events: &Sender<Event>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };

        if value.get("method").and_then(|m| m.as_str()) == Some(gmuse_proto::event::EVENT_METHOD) {
            if let Ok(event) = serde_json::from_value::<Event>(value["params"].clone())
                && events.send(event).is_err()
            {
                break; // nobody is listening for events any more
            }
            continue;
        }

        let Ok(id) = serde_json::from_value::<Id>(value["id"].clone()) else {
            // A response with no usable id answers nothing anyone asked — a
            // parse error against a line the engine could not read that far
            // into. Nothing to correlate it with, so it is dropped rather than
            // delivered to an arbitrary waiter.
            continue;
        };
        let Some(waiter) = pending.lock().expect("pending").remove(&id) else {
            continue;
        };
        let outcome = if let Some(error) = value.get("error") {
            Err(
                serde_json::from_value::<RpcError>(error.clone()).unwrap_or_else(|e| {
                    RpcError::new(gmuse_proto::error::code::INTERNAL, e.to_string())
                }),
            )
        } else {
            Ok(value
                .get("result")
                .cloned()
                .unwrap_or(serde_json::Value::Null))
        };
        let _ = waiter.send(outcome);
    }
    // The socket closed. Dropping every waiter's sender turns their `recv` into
    // `Closed` rather than leaving them blocked for ever.
    pending.lock().expect("pending").clear();
}
