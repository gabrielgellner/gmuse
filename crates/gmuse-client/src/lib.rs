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
use serde_json::value::RawValue;

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
    /// **Something is listening and not talking** — the engine at `path` accepted the
    /// connection and never answered `hello`.
    ///
    /// Its own variant rather than [`ClientError::TimedOut`], because the two ask for
    /// opposite responses: a timeout on an ordinary call is worth asking again, and this
    /// is worth stopping. A wedged engine holds the socket and, having bound it, the lock
    /// — so nothing a client does reaches it and no second engine can start. The only
    /// repair is to stop it, which is why the message names its pid when the lock beside
    /// the socket still says.
    #[error(
        "an engine at {path} accepts connections and does not answer{} — `gmuse ctl` cannot reach it either; stop it{} and try again",
        match pid { Some(p) => format!(" (pid {p})"), None => String::new() },
        match pid { Some(p) => format!(" (kill {p})"), None => String::new() },
    )]
    Unresponsive { path: String, pid: Option<u32> },
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

/// **A reply is carried unparsed** — #261.
///
/// The demultiplexer correlates by id and has no idea what the caller expects, so it used to
/// parse every reply into a `serde_json::Value` and clone the `result` out of it. For a
/// 4.5 MB reply that is a `Value` tree several times the size of the text, built to be thrown
/// away — and an attached view's memory follows the peak, so `listened` over the wire made the
/// process *larger* than folding the log had.
///
/// A [`RawValue`] is the bytes with their span remembered. The caller deserialises it into
/// whatever it wanted, once.
type Pending = Arc<Mutex<HashMap<Id, Sender<Result<Box<RawValue>, RpcError>>>>>;

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

        // **Bounded, because accepting is not answering** — #263.
        //
        // `attach::is_live` asks only whether a connection is accepted, and an engine spinning
        // at 100% of a core accepts and then says nothing (#264). Every caller then hung here,
        // in the handshake, before it had asked for anything: a `gmuse ctl`, a view attaching,
        // and a `shelf import` that sat for thirty minutes at 0% CPU with its files already
        // moved.
        //
        // **Here and not at each call site**, because the handshake is the one exchange every
        // client makes and the only one it makes before it can know anything is wrong.
        //
        // Five seconds is not a performance budget. A handshake is one line each way off a
        // unix socket; anything that cannot manage it in five seconds is not going to.
        const HANDSHAKE: std::time::Duration = std::time::Duration::from_secs(5);
        let params = HelloParams::new(name, version);
        let result = client
            .call_within("hello", Some(serde_json::to_value(&params)?), HANDSHAKE)
            // **Named for what it is, not for how long it took.** A handshake that times
            // out is a listener that will not talk, and every caller wants to say so and
            // stop — not report five seconds and try again.
            .map_err(|e| match e {
                ClientError::TimedOut(_) => ClientError::Unresponsive {
                    path: path.display().to_string(),
                    pid: None,
                },
                other => other,
            })?;
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
        self.typed(method, params, None)
    }

    /// **The same call, deserialised into what the caller wanted** — #261.
    ///
    /// [`Client::call`] builds a `serde_json::Value` on the way, which for a large reply is a
    /// tree several times the size of the text and is then thrown away. This one goes from the
    /// wire to `T`: `query.listened` is 4.5 MB, and the difference between these two is whether
    /// the process keeps a high-water mark to match.
    pub fn call_typed<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<T, ClientError> {
        self.typed(method, params, None)
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
        self.typed(method, params, Some(within))
    }

    fn typed<T: serde::de::DeserializeOwned>(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        within: Option<Duration>,
    ) -> Result<T, ClientError> {
        let raw = self.ask(method, params, within)?;
        serde_json::from_str(raw.get()).map_err(|e| {
            ClientError::Refused(RpcError::new(
                gmuse_proto::error::code::INTERNAL,
                format!("the reply is not what was expected: {e}"),
            ))
        })
    }

    fn ask(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
        within: Option<Duration>,
    ) -> Result<Box<RawValue>, ClientError> {
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

    /// Wait for one event, or say why not.
    ///
    /// **Two failures, and collapsing them cost 100% of a core** — #264. This
    /// mapped every `recv_timeout` error to `TimedOut`, but the channel fails two
    /// ways: it times out, or it is *disconnected* because `demultiplex` returned
    /// and dropped the only `Sender`. A disconnected `recv_timeout` returns **at
    /// once**, so a caller that retries on error — which is the obvious way to use
    /// a function whose only error means "nothing yet" — spins as fast as the
    /// scheduler allows.
    ///
    /// That is what a view whose engine had gone did: `gmuse-tui-playback` burned
    /// a core for 20 minutes of a 37-minute session, entirely inside
    /// `Instant::now`, because each pass computed a 250 ms deadline it then never
    /// waited on.
    ///
    /// `Closed` is the existing name for the far end being gone, so a caller can
    /// tell "ask again" from "there is nobody to ask".
    pub fn next_event(&self, within: Duration) -> Result<Event, ClientError> {
        self.events.recv_timeout(within).map_err(|e| match e {
            crossbeam_channel::RecvTimeoutError::Timeout => ClientError::TimedOut(within),
            crossbeam_channel::RecvTimeoutError::Disconnected => ClientError::Closed,
        })
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
    // **The envelope, and the payload left alone** — #261. This used to parse every line into a
    // `serde_json::Value` and clone the `result` out of it; for a 4.5 MB reply that is a tree
    // several times the size of the text, built here to be discarded by the caller. What it
    // needs is the id and which arm it is, and `RawValue` gives it those without reading the
    // rest.
    #[derive(serde::Deserialize)]
    struct Line<'a> {
        #[serde(default, borrow)]
        method: Option<&'a str>,
        #[serde(default, borrow)]
        params: Option<&'a RawValue>,
        #[serde(default)]
        id: Option<Id>,
        #[serde(default, borrow)]
        result: Option<&'a RawValue>,
        #[serde(default)]
        error: Option<RpcError>,
    }

    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(parsed) = serde_json::from_str::<Line>(&line) else {
            continue;
        };

        if parsed.method == Some(gmuse_proto::event::EVENT_METHOD) {
            if let Some(params) = parsed.params
                && let Ok(event) = serde_json::from_str::<Event>(params.get())
                && events.send(event).is_err()
            {
                break; // nobody is listening for events any more
            }
            continue;
        }

        let Some(id) = parsed.id else {
            // A response with no usable id answers nothing anyone asked — a
            // parse error against a line the engine could not read that far
            // into. Nothing to correlate it with, so it is dropped rather than
            // delivered to an arbitrary waiter.
            continue;
        };
        let Some(waiter) = pending.lock().expect("pending").remove(&id) else {
            continue;
        };
        let outcome = match parsed.error {
            Some(error) => Err(error),
            // A reply with no `result` at all is `null`, which is what a method that answers
            // nothing sends and what `()` deserialises from.
            None => Ok(parsed
                .result
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| RawValue::from_string("null".into()).expect("null is json"))),
        };
        let _ = waiter.send(outcome);
    }
    // The socket closed. Dropping every waiter's sender turns their `recv` into
    // `Closed` rather than leaving them blocked for ever.
    pending.lock().expect("pending").clear();
}
