//! **The engine's front door** — #214, §4.2, §7.1, §7.3.
//!
//! A unix socket, an NDJSON envelope, and a handshake that refuses a client
//! speaking a different major version.
//!
//! ## This generalises `remote.rs` rather than replacing it
//!
//! `remote.rs` is not a sketch: it has a listener thread off the audio path, a
//! stale-socket story that is correct by construction, and status answered from
//! a published snapshot so a status bar can never wedge a render. Those are the
//! two structural ideas the engine needs, and they are kept. What changes is the
//! framing — a line of the `:` language becomes a JSON-RPC object — and that
//! this is on its way to being the *only* way in.
//!
//! `--remote` is still here. §4.2 retires it in favour of `gmuse ctl`, and
//! `gmuse ctl` is #217: removing the old door before the new one opens would
//! leave a release with no way to drive a running player from a key binding.
//!
//! ## The stale socket needs no liveness check
//!
//! `UnixListener::bind` fails if the path exists, which normally means guessing
//! whether a leftover socket belongs to a live process. It does not here, for
//! the reason `remote.rs` already records: **the lock is taken before the bind**,
//! so holding it proves no other engine is running and any socket present is
//! stale by construction. The kernel made the liveness check, once.
//!
//! It is worth being precise about why the alternative is worse. Connecting to
//! the socket to see whether anything answers is a race — nothing answers
//! between a crash and a restart either — and it is also a lie about what was
//! learned: a refused connection means nothing is *listening*, not that nothing
//! is *running*. The lock answers the question that was actually being asked.

pub mod attach;
pub mod client;
pub mod control;
pub mod librarian;
pub mod library;
pub mod outbox;
pub mod queue;
pub mod socket;

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gmuse_proto::envelope::{self, MAX_LINE};
use gmuse_proto::hello::{HelloParams, HelloResult};
use gmuse_proto::request::method;
use gmuse_proto::{Call, Event, Response, RpcError, Topic, error::code, version};
use thiserror::Error;

/// What the engine could not do.
#[derive(Debug, Error)]
pub enum EngineError {
    #[error("another gmuse engine is running{}", match .pid { Some(p) => format!(" (pid {p})"), None => String::new() })]
    Running { pid: Option<u32> },
    #[error("could not listen on {path}: {source}")]
    Bind {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("could not prepare {path}: {source}")]
    Prepare {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// What the engine tells a client about itself at `hello`.
///
/// Held rather than recomputed per connection so that `engine_generation` is
/// stable for the engine's life, which is the whole point of it (§7.6).
#[derive(Debug, Clone)]
pub struct Identity {
    pub engine_generation: String,
    pub sample_rate: u32,
    pub capabilities: Vec<String>,
}

impl Identity {
    /// **A fresh generation per engine, from the clock, the pid and a counter.**
    ///
    /// Deliberately not a random number — that would mean a dependency for no
    /// gain — and deliberately not the pid alone, which a system recycles.
    ///
    /// **The counter is not belt-and-braces.** The first version was clock and
    /// pid, on the reasoning that "the same pid cannot recur within the same
    /// nanosecond". That is true across processes and false within one: two
    /// identities built back to back share a pid by definition, and on a fast
    /// machine they share the nanosecond too. A test asking for two and getting
    /// one is what found it. Two engines cannot both hold the lock, so this
    /// could not have collided in production today — but `engine_generation`
    /// exists so a client can tell that its references expired, and a value
    /// that is only usually unique is not one you can build that on.
    pub fn new(sample_rate: u32, capabilities: Vec<String>) -> Self {
        static SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let seq = SEQ.fetch_add(1, Ordering::Relaxed);
        Self {
            engine_generation: format!("{:x}-{:x}-{:x}", now, std::process::id(), seq),
            sample_rate,
            capabilities,
        }
    }
}

/// What a connection can ask the running engine.
///
/// **A trait with one implementor is a struct** (§12), and this is the
/// exception the rule allows for: the daemon must be startable in a test
/// without an audio device, a library or a decoder. It is the seam that lets
/// `hello` be tested against a stub and served by the real engine.
pub trait Answers: Send + Sync + 'static {
    /// Which audio stream the frame clock is counting in (§10.1, #212). Zero
    /// before any stream has opened.
    fn stream_generation(&self) -> u32;
    /// The engine's clock, in nanoseconds, for offset estimation (§7.9).
    fn now_ns(&self) -> i64;
    /// Answer anything that is not `hello`.
    ///
    /// **Typed** — I4. The string and the `Value` were consumed at the
    /// boundary; what arrives here is a [`Call`], and an implementor that
    /// wanted to match on a method name again would find there is none left.
    fn call(&self, request: Call) -> Result<serde_json::Value, RpcError>;
}

/// A running engine. Dropping it stops listening and removes the socket.
pub struct Engine {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    subscribers: Arc<client::Subscribers>,
    _lock: crate::lock::Lock,
}

impl Engine {
    /// Where it is listening.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Say something to every view subscribed to the topic.
    ///
    /// **Returns rather than waits** — I1. The count is how many clients the
    /// event was queued for, not how many have received it; a client is sent to
    /// by its own writer thread, on its own time.
    pub fn publish(&self, event: &Event) -> usize {
        self.subscribers.publish(event)
    }

    /// How many views are connected.
    pub fn clients(&self) -> usize {
        self.subscribers.len()
    }

    /// Wait, briefly, for what is queued to reach the sockets.
    ///
    /// **For shutdown, and only for shutdown.** [`Engine::publish`] returns
    /// without waiting because I1 forbids blocking on a view — that holds while
    /// the engine is running. Stopping is the one moment where the alternative
    /// is worse: a `stopping` notice queued and then dropped on the floor as
    /// the process exits tells a view nothing, and it cannot distinguish that
    /// from a crash.
    ///
    /// Bounded, so a client that has stopped reading delays the exit by the
    /// deadline and not for ever. Returns whether everything drained.
    pub fn drain(&self, within: std::time::Duration) -> bool {
        let deadline = std::time::Instant::now() + within;
        while std::time::Instant::now() < deadline {
            if self.subscribers.pending() == 0 {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        false
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        // **Wake the accept loop by connecting to it.** `UnixListener::accept`
        // blocks, and nothing else will return it: without this the thread sits
        // in the kernel until a client happens to arrive, and the join below
        // waits with it.
        let _ = UnixStream::connect(&self.path);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Take the single-instance lock, before anything expensive happens.
///
/// **Separate from [`serve`] so a caller can claim the engine first.** The lock
/// is what decides whether this process is *the* engine, and until #219 the
/// answer arrived after the audio device had already been opened — so a second
/// engine claimed the hardware, printed the rate it had opened it at, and only
/// then discovered it was not allowed to be an engine. On a real device that is
/// a claim on somebody's sound card for the length of a failed startup.
///
/// The ordering it establishes is also the stale-socket story: holding this
/// proves no other engine is listening, so any socket file present is a
/// leftover (see [`serve`]).
pub fn claim(path: &Path) -> Result<crate::lock::Lock, EngineError> {
    let dir = path.parent().unwrap_or(Path::new("."));
    socket::prepare_dir(dir).map_err(|source| EngineError::Prepare {
        path: dir.to_path_buf(),
        source,
    })?;
    crate::lock::acquire(&dir.join(socket::LOCK)).map_err(|e| match e {
        crate::lock::LockError::Held { pid, .. } => EngineError::Running { pid },
        crate::lock::LockError::Io { path, source } => EngineError::Prepare { path, source },
    })
}

/// Take the lock, bind the socket, and start answering.
pub fn serve(
    path: &Path,
    identity: Identity,
    answers: Arc<dyn Answers>,
) -> Result<Engine, EngineError> {
    let lock = claim(path)?;
    serve_with(path, identity, answers, lock)
}

/// As [`serve`], with the lock already held — see [`claim`].
pub fn serve_with(
    path: &Path,
    identity: Identity,
    answers: Arc<dyn Answers>,
    lock: crate::lock::Lock,
) -> Result<Engine, EngineError> {
    let _ = path.parent();

    // Holding the lock proves nothing else is listening, so whatever is here is
    // a leftover. No liveness check, because it has already been made.
    let _ = std::fs::remove_file(path);

    let listener = UnixListener::bind(path).map_err(|source| EngineError::Bind {
        path: path.to_path_buf(),
        source,
    })?;
    socket::restrict(path).map_err(|source| EngineError::Prepare {
        path: path.to_path_buf(),
        source,
    })?;

    let stop = Arc::new(AtomicBool::new(false));
    let stopping = Arc::clone(&stop);
    // Everyone connected, so an event reaches every view that asked for it.
    let subscribers = Arc::new(client::Subscribers::default());
    let published = Arc::clone(&subscribers);
    let thread = std::thread::Builder::new()
        .name("gmuse-engine-listener".into())
        .spawn(move || {
            for stream in listener.incoming() {
                if stopping.load(Ordering::Acquire) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let answers = Arc::clone(&answers);
                let subscribers = Arc::clone(&subscribers);
                let identity = identity.clone();
                // A thread per connection. There are a handful of views, not a
                // thousand, and a blocking read per client is the simplest
                // thing that cannot make one client's slowness another's.
                let _ = std::thread::Builder::new()
                    .name("gmuse-engine-conn".into())
                    .spawn(move || converse(stream, &identity, answers, &subscribers));
            }
        })
        .map_err(|source| EngineError::Bind {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(Engine {
        path: path.to_path_buf(),
        stop,
        thread: Some(thread),
        subscribers: published,
        _lock: lock,
    })
}

/// One connection, from `hello` to close.
///
/// **Three parts, and the split is I1.** The reader parses; a worker answers;
/// the writer is the only thing that touches the socket. No path from the
/// engine to a client write exists that does not go through the bounded
/// [`client::Outbox`], which is what makes "the engine never blocks on a view"
/// structural rather than a rule to remember.
fn converse(
    stream: UnixStream,
    identity: &Identity,
    answers: Arc<dyn Answers>,
    subscribers: &Arc<client::Subscribers>,
) {
    let Ok(write_half) = stream.try_clone() else {
        return;
    };

    let conn = Arc::new(client::Client::new(client::DEFAULT_CAPACITY));
    subscribers.add(Arc::clone(&conn));

    // The writer. It outlives the reader by design: a client that stops sending
    // is still owed the answers it already asked for.
    let writer = {
        let conn = Arc::clone(&conn);
        std::thread::Builder::new()
            .name("gmuse-engine-writer".into())
            .spawn(move || {
                let mut out = std::io::BufWriter::new(write_half);
                while let Some(item) = conn.next() {
                    let body = match &item {
                        client::Outgoing::Response(r) => serde_json::to_string(r),
                        client::Outgoing::Event(e) => {
                            serde_json::to_string(&gmuse_proto::event::notification(e))
                        }
                    };
                    let Ok(body) = body else { continue };
                    if writeln!(out, "{body}").is_err() || out.flush().is_err() {
                        // The client has gone. Not an error: §7.10 calls a
                        // disconnect a non-event for the engine.
                        conn.close();
                        return;
                    }
                }
            })
    };

    let mut reader = BufReader::new(stream);
    let mut greeted = false;
    let mut workers = Vec::new();

    loop {
        // **Read at most one line's worth, then decide.** `lines()` and
        // `read_line` both grow their buffer until a newline arrives, so a
        // client that never sends one grows the engine's memory instead — which
        // is the thing §7.1's cap exists to prevent, and a cap checked after
        // the read would be a cap on nothing.
        let mut raw = Vec::new();
        let read = (&mut reader)
            .take(MAX_LINE as u64 + 1)
            .read_until(b'\n', &mut raw);
        let Ok(read) = read else { break };
        if read == 0 {
            break; // the client hung up
        }
        if raw.len() > MAX_LINE {
            // §7.1: rejected *and discarded* — the tail is not a request, so
            // skipping to the next newline is what keeps the stream usable
            // rather than reading the remainder as a stream of parse errors.
            conn.respond(Response::err(
                None,
                RpcError::invalid_request(format!("line exceeds the {MAX_LINE}-byte maximum")),
            ));
            if discard_line(&mut reader).is_err() {
                break;
            }
            continue;
        }
        let Ok(line) = String::from_utf8(raw) else {
            conn.respond(Response::err(None, RpcError::parse("a line must be UTF-8")));
            continue;
        };
        if line.trim().is_empty() {
            continue;
        }

        // **Session work is answered here; everything else is dispatched.**
        //
        // §7.2 requires that responses have no ordering guarantee, and calls it
        // a requirement rather than a permission: a reader that answered each
        // line before reading the next would make a cue press wait behind a
        // scan, which is the head-of-line blocking I6 forbids. The handshake
        // and the subscription calls are the exception because they *are* the
        // connection's own state — running them off-thread would let a client
        // race its own `hello`.
        if is_session_work(&line) || !greeted {
            let (reply, fatal) =
                serve_line(&line, identity, answers.as_ref(), &mut greeted, Some(&conn));
            if let Some(reply) = reply {
                conn.respond(reply);
            }
            if fatal {
                break;
            }
            continue;
        }

        let conn_for_worker = Arc::clone(&conn);
        let answers = Arc::clone(&answers);
        let identity = identity.clone();
        let worker = std::thread::Builder::new()
            .name("gmuse-engine-worker".into())
            .spawn(move || {
                let mut already = true;
                let (reply, _) = serve_line(
                    &line,
                    &identity,
                    answers.as_ref(),
                    &mut already,
                    Some(&conn_for_worker),
                );
                if let Some(reply) = reply {
                    conn_for_worker.respond(reply);
                }
            });
        if let Ok(worker) = worker {
            workers.push(worker);
        }
    }

    // The reader is done. Let the outstanding answers finish and be written
    // before the socket goes, or a client that asked and then stopped sending
    // would never hear back.
    for worker in workers {
        let _ = worker.join();
    }
    conn.close();
    if let Ok(writer) = writer {
        let _ = writer.join();
    }
}

/// Whether a line is the connection's own business rather than the engine's.
///
/// A cheap prefix test on the raw line, deliberately: it decides *where* to
/// parse, so it cannot be the thing that parses. Anything it misjudges is still
/// parsed correctly one layer down — the cost of a wrong guess is a thread, not
/// a wrong answer.
fn is_session_work(line: &str) -> bool {
    [
        gmuse_proto::request::method::HELLO,
        gmuse_proto::request::method::SUBSCRIBE,
        gmuse_proto::request::method::UNSUBSCRIBE,
    ]
    .iter()
    .any(|m| line.contains(&format!("\"{m}\"")))
}

/// Throw away everything up to and including the next newline.
fn discard_line(reader: &mut impl BufRead) -> std::io::Result<()> {
    let mut sink = Vec::new();
    loop {
        sink.clear();
        let n = (&mut *reader)
            .take(MAX_LINE as u64)
            .read_until(b'\n', &mut sink)?;
        if n == 0 || sink.last() == Some(&b'\n') {
            return Ok(());
        }
    }
}

/// The line server, for the golden transcripts — #215.
///
/// **Exposed rather than duplicated.** A transcript suite that replayed against
/// its own copy of the dispatch would pin that copy, which is the one thing it
/// must not do.
pub fn serve_line_for_test(
    line: &str,
    identity: &Identity,
    answers: &dyn Answers,
    greeted: &mut bool,
    client: &client::Client,
) -> (Option<Response>, bool) {
    serve_line(line, identity, answers, greeted, Some(client))
}

/// Answer one line. Returns the reply to send, and whether to close after it.
///
/// Split out from the socket so it can be tested without one: everything about
/// the protocol is decided here, and nothing here knows what a file descriptor
/// is.
fn serve_line(
    line: &str,
    identity: &Identity,
    answers: &dyn Answers,
    greeted: &mut bool,
    client: Option<&client::Client>,
) -> (Option<Response>, bool) {
    let request = match envelope::parse_line(line) {
        Ok(r) => r,
        Err(e) => return (Some(Response::err(None, e)), false),
    };
    let id = request.id.clone();
    let notification = request.is_notification();

    // **`hello` must come first** (§7.3). A client that skips it has not agreed
    // a version, so anything the engine returned would be in a shape neither
    // side has promised.
    if !*greeted && request.method != method::HELLO {
        let e = RpcError::invalid_request(format!(
            "the first message must be hello, got {:?}",
            request.method
        ));
        return (Some(Response::err(id, e)), true);
    }

    // **The one parse boundary** (I4, §7.5). After this line the method is a
    // type: nothing below matches on a string or reaches into a `Value`.
    let call = match Call::of(&request.method, request.params) {
        Ok(call) => call,
        // **A notification gets silence even when it is wrong** (§7.2). The
        // boundary refusing an unknown method is still a refusal of something
        // that asked for no answer, and replying anyway would send a response
        // carrying `"id": null` that no client has anywhere to put.
        Err(e) => return ((!notification).then(|| Response::err(id, e)), false),
    };

    let outcome = if let Call::Hello(params) = &call {
        match hello(params, identity, answers) {
            Ok(result) => {
                *greeted = true;
                Ok(result)
            }
            Err(e) => {
                // A version mismatch closes the connection (§7.3).
                let close = e.code == code::INVALID_PARAMS;
                let reply = (!notification).then(|| Response::err(id, e));
                return (reply, close);
            }
        }
    } else if let Call::Subscribe { topics } = &call {
        // **Handled here, not by `Answers`.** A subscription is per-connection
        // bookkeeping, and `Answers` is what the *engine* can answer — giving
        // it a client to mutate would put connection state behind a seam whose
        // whole purpose is not to have any.
        Ok(subscription_result(client, |c| c.subscribe(topics)))
    } else if let Call::Unsubscribe { topics } = &call {
        Ok(subscription_result(client, |c| c.unsubscribe(topics)))
    } else {
        answers.call(call)
    };

    if notification {
        // Fire-and-forget: §7.2 allows it, so an error here has nowhere to go.
        return (None, false);
    }
    match outcome {
        Ok(result) => (Some(Response::ok(id, result)), false),
        Err(e) => (Some(Response::err(id, e)), false),
    }
}

/// What a `sub.*` call answers: the topics now in force.
///
/// **The full set, not the delta.** A client that asked for two topics and got
/// back "ok" would have to model the engine's view of its own subscription;
/// answering with the whole list means it never has to.
fn subscription_result(
    client: Option<&client::Client>,
    change: impl FnOnce(&client::Client) -> Vec<Topic>,
) -> serde_json::Value {
    let topics = client.map(change).unwrap_or_default();
    serde_json::json!({ "topics": topics })
}

/// The handshake.
fn hello(
    params: &HelloParams,
    identity: &Identity,
    answers: &dyn Answers,
) -> Result<serde_json::Value, RpcError> {
    version::accept(&params.protocol)
        .map_err(|e| RpcError::invalid_params("protocol", e.to_string()))?;

    let result = HelloResult {
        protocol: version::PROTOCOL.into(),
        engine_generation: identity.engine_generation.clone(),
        stream_generation: answers.stream_generation(),
        engine_now_ns: answers.now_ns(),
        sample_rate: identity.sample_rate,
        capabilities: identity.capabilities.clone(),
    };
    serde_json::to_value(result).map_err(|e| RpcError::new(code::INTERNAL, e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An engine with nothing behind the door, which is what #214 ships.
    struct Stub {
        generation: u32,
    }
    impl Answers for Stub {
        fn stream_generation(&self) -> u32 {
            self.generation
        }
        fn now_ns(&self) -> i64 {
            1_234
        }
        fn call(&self, request: Call) -> Result<serde_json::Value, RpcError> {
            Ok(serde_json::json!(request.method()))
        }
    }

    fn identity() -> Identity {
        Identity::new(48_000, vec!["dj".into()])
    }

    /// Drive `serve_line` without a socket.
    fn exchange(lines: &[&str]) -> Vec<Option<Response>> {
        let id = identity();
        let answers = Stub { generation: 3 };
        let mut greeted = false;
        let mut out = Vec::new();
        for line in lines {
            let (reply, fatal) = serve_line(line, &id, &answers, &mut greeted, None);
            out.push(reply);
            if fatal {
                break;
            }
        }
        out
    }

    const HELLO: &str =
        r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/0"}}"#;

    #[test]
    fn the_handshake_answers_with_what_the_client_needs_to_extrapolate() {
        let replies = exchange(&[HELLO]);
        let result = replies[0].as_ref().unwrap().result.as_ref().unwrap();
        assert_eq!(result["protocol"], "gmuse/0");
        assert_eq!(result["stream_generation"], 3);
        assert_eq!(result["sample_rate"], 48_000);
        assert_eq!(result["capabilities"][0], "dj");
        assert!(
            result["engine_generation"]
                .as_str()
                .is_some_and(|s| !s.is_empty()),
            "a generation that scopes references cannot be empty"
        );
    }

    /// §7.3: a differing major is an error **and the connection closes**.
    /// Answering anything else would be answering in a shape neither side has
    /// agreed on.
    #[test]
    fn a_client_speaking_another_major_is_refused_and_cut_off() {
        let id = identity();
        let answers = Stub { generation: 0 };
        let mut greeted = false;
        let (reply, fatal) = serve_line(
            r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/9"}}"#,
            &id,
            &answers,
            &mut greeted,
            None,
        );
        assert!(fatal, "a version mismatch closes the connection");
        assert!(!greeted);
        let error = reply.unwrap().error.unwrap();
        assert_eq!(error.code, code::INVALID_PARAMS);
        assert!(error.message.contains("gmuse/9"), "{}", error.message);
    }

    #[test]
    fn nothing_is_served_before_hello() {
        let replies = exchange(&[r#"{"jsonrpc":"2.0","id":1,"method":"echo"}"#]);
        let error = replies[0].as_ref().unwrap().error.as_ref().unwrap();
        assert_eq!(error.code, code::INVALID_REQUEST);
        assert!(error.message.contains("hello"), "{}", error.message);
    }

    #[test]
    fn after_hello_an_unknown_method_is_an_error_and_not_a_disconnection() {
        let replies = exchange(&[HELLO, r#"{"jsonrpc":"2.0","id":2,"method":"nope"}"#]);
        assert_eq!(replies.len(), 2, "the connection stayed open");
        assert_eq!(
            replies[1].as_ref().unwrap().error.as_ref().unwrap().code,
            code::METHOD_NOT_FOUND
        );
    }

    /// §7.2: a notification gets no reply, even when it fails.
    #[test]
    fn a_notification_is_answered_with_silence() {
        let replies = exchange(&[HELLO, r#"{"jsonrpc":"2.0","method":"nope"}"#]);
        assert!(replies[1].is_none());
    }

    #[test]
    fn a_batch_is_refused_before_the_handshake_and_does_not_close_the_connection() {
        let replies = exchange(&[r#"[{"jsonrpc":"2.0","id":1,"method":"hello"}]"#, HELLO]);
        assert_eq!(
            replies[0].as_ref().unwrap().error.as_ref().unwrap().code,
            code::INVALID_REQUEST
        );
        assert!(
            replies[1].as_ref().unwrap().result.is_some(),
            "hello still worked"
        );
    }

    /// The whole point of `engine_generation`: two engines must not claim the
    /// same one, or a client cannot tell that its references expired.
    #[test]
    fn two_engines_do_not_share_a_generation() {
        assert_ne!(
            Identity::new(48_000, vec![]).engine_generation,
            Identity::new(48_000, vec![]).engine_generation
        );
    }
}

#[cfg(test)]
mod over_a_real_socket {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    struct Bare;
    impl Answers for Bare {
        fn stream_generation(&self) -> u32 {
            7
        }
        fn now_ns(&self) -> i64 {
            0
        }
        fn call(&self, request: Call) -> Result<serde_json::Value, RpcError> {
            Err(RpcError::method_not_found(request.method()))
        }
    }

    /// A directory of this test's own, so two tests never share a socket.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gmuse-engine-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn ask(path: &Path, line: &str) -> String {
        let mut stream = UnixStream::connect(path).expect("connect");
        writeln!(stream, "{line}").unwrap();
        stream.flush().unwrap();
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).unwrap();
        reply
    }

    const HELLO: &str =
        r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/0"}}"#;

    #[test]
    fn the_daemon_binds_and_handshakes() {
        let dir = scratch("handshake");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(44_100, vec![]), Arc::new(Bare)).unwrap();

        let reply: serde_json::Value = serde_json::from_str(&ask(&path, HELLO)).unwrap();
        assert_eq!(reply["result"]["protocol"], "gmuse/0");
        assert_eq!(reply["result"]["stream_generation"], 7);
        assert_eq!(reply["result"]["sample_rate"], 44_100);

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The socket and its directory are this user's** — §7.1.
    #[test]
    fn the_directory_is_0700_and_the_socket_0600() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("modes");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare)).unwrap();

        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700, "directory");
        assert_eq!(mode(&path), 0o600, "socket");

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **Single instance, by the lock and not by the socket** — §7.1.
    #[test]
    fn a_second_engine_is_refused_while_the_first_holds_the_lock() {
        let dir = scratch("single");
        let path = dir.join(socket::SOCKET);
        let first = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare)).unwrap();

        let second = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare));
        assert!(
            matches!(second, Err(EngineError::Running { .. })),
            "a second engine must not bind over the first"
        );
        // And the first is still answering, rather than having had its socket
        // removed by the attempt.
        assert!(ask(&path, HELLO).contains("result"));

        drop(first);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **A leftover socket is reclaimed without a liveness check** — the
    /// Done-when line, and the reason is the ordering: the lock is taken first,
    /// so holding it *is* the proof that the file is stale. A crashed engine
    /// leaves exactly this — a socket nothing is listening on — and asking it
    /// whether it is alive would be a race with its own restart.
    #[test]
    fn a_stale_socket_is_reclaimed_because_the_lock_already_answered() {
        let dir = scratch("stale");
        let path = dir.join(socket::SOCKET);

        // What a killed engine leaves behind: a socket file, no listener, no
        // lock held.
        std::fs::write(&path, b"").unwrap();
        assert!(path.exists());
        assert!(
            UnixStream::connect(&path).is_err(),
            "nothing is listening on the leftover"
        );

        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare))
            .expect("the stale socket is reclaimed, not fatal");
        assert!(ask(&path, HELLO).contains("result"));

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Dropping the engine takes the socket with it, so the next start is a
    /// clean bind rather than a reclaim.
    #[test]
    fn stopping_removes_the_socket() {
        let dir = scratch("cleanup");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare)).unwrap();
        assert!(path.exists());
        drop(engine);
        assert!(!path.exists(), "a stopped engine leaves no socket");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// §7.1's cap, over a real stream: the oversized line is refused and
    /// *discarded*, so the request after it is still answered.
    #[test]
    fn an_oversized_line_is_refused_and_the_connection_survives_it() {
        let dir = scratch("toolong");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Bare)).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{}", "x".repeat(MAX_LINE + 10)).unwrap();
        writeln!(stream, "{HELLO}").unwrap();
        stream.flush().unwrap();

        let mut lines = BufReader::new(stream).lines();
        let first: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(first["error"]["code"], code::INVALID_REQUEST);
        let second: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(
            second["result"]["protocol"], "gmuse/0",
            "the hello after the discarded line was still served"
        );

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

/// The two properties #216 exists to establish, over real sockets.
#[cfg(test)]
mod under_pressure {
    use super::*;
    use std::io::{BufRead, BufReader, Write};

    /// An engine where one method is slow and another is not.
    struct Uneven;
    impl Answers for Uneven {
        fn stream_generation(&self) -> u32 {
            0
        }
        fn now_ns(&self) -> i64 {
            0
        }
        fn call(&self, request: Call) -> Result<serde_json::Value, RpcError> {
            match request {
                // Stands in for a scan or a query on the librarian.
                Call::EngineStatus => {
                    std::thread::sleep(std::time::Duration::from_millis(300));
                    Ok(serde_json::json!("slow"))
                }
                // Stands in for a cue press.
                Call::TimeNow => Ok(serde_json::json!("fast")),
                other => Err(RpcError::method_not_found(other.method())),
            }
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gmuse-216-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const HELLO: &str =
        r#"{"jsonrpc":"2.0","id":1,"method":"hello","params":{"protocol":"gmuse/0"}}"#;

    /// **§7.2, as a requirement rather than a permission.** A reader that
    /// answered each line before reading the next would make a cue press wait
    /// behind a scan — the head-of-line blocking I6 forbids. The slow request
    /// goes first and must come back second.
    #[test]
    fn a_fast_response_overtakes_a_slow_one() {
        let dir = scratch("overtake");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Uneven)).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{HELLO}").unwrap();
        writeln!(
            stream,
            r#"{{"jsonrpc":"2.0","id":2,"method":"engine.status"}}"#
        )
        .unwrap();
        writeln!(stream, r#"{{"jsonrpc":"2.0","id":3,"method":"time.now"}}"#).unwrap();
        stream.flush().unwrap();

        let mut ids = Vec::new();
        for line in BufReader::new(stream).lines().take(3) {
            let v: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
            ids.push(v["id"].as_i64().unwrap());
        }

        assert_eq!(
            ids,
            vec![1, 3, 2],
            "the fast answer came back before the slow one it was sent after"
        );

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The slow client** — §7.10. It subscribes, never reads, and is driven
    /// past its bound; what it eventually finds waiting is a resync notice
    /// rather than a backlog.
    #[test]
    fn a_client_that_stops_reading_is_coalesced_then_told_to_resync() {
        let dir = scratch("slow");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Uneven)).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{HELLO}").unwrap();
        writeln!(
            stream,
            r#"{{"jsonrpc":"2.0","id":2,"method":"sub.subscribe","params":{{"topics":["params"]}}}}"#
        )
        .unwrap();
        stream.flush().unwrap();

        // Read only the two answers, then stop reading entirely.
        let mut reading = BufReader::new(stream.try_clone().unwrap());
        for _ in 0..2 {
            let mut line = String::new();
            reading.read_line(&mut line).unwrap();
        }

        // Wait for the subscription to be in force before publishing, or the
        // test would be racing its own setup rather than the backpressure.
        for _ in 0..200 {
            if engine.clients() > 0
                && engine.publish(&Event::new(Topic::Params, serde_json::json!({"warm":true}))) > 0
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        // **Distinct keys**, so nothing coalesces and the bound is genuinely
        // reached. Coalescing is covered by the outbox's own tests; what this
        // asserts is that the engine did not wait for a client that stopped.
        let started = std::time::Instant::now();
        for n in 0..10_000 {
            engine.publish(&Event::keyed(
                Topic::Params,
                n.to_string(),
                serde_json::json!({ "n": n }),
            ));
        }
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "publishing to a client that stopped reading blocked the engine (I1)"
        );

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A subscription answers with the topics now in force, so a client never
    /// has to model the engine's view of its own state.
    #[test]
    fn subscribing_answers_with_the_whole_set() {
        let dir = scratch("subs");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Uneven)).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{HELLO}").unwrap();
        writeln!(
            stream,
            r#"{{"jsonrpc":"2.0","id":2,"method":"sub.subscribe","params":{{"topics":["queue","library"]}}}}"#
        )
        .unwrap();
        writeln!(
            stream,
            r#"{{"jsonrpc":"2.0","id":3,"method":"sub.unsubscribe","params":{{"topics":["queue"]}}}}"#
        )
        .unwrap();
        stream.flush().unwrap();

        let mut lines = BufReader::new(stream).lines();
        let _hello = lines.next().unwrap().unwrap();
        let sub: serde_json::Value = serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(
            sub["result"]["topics"],
            serde_json::json!(["queue", "library"])
        );
        let unsub: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(unsub["result"]["topics"], serde_json::json!(["library"]));

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// An event reaches a subscribed client as a notification with no id.
    #[test]
    fn a_subscribed_client_receives_events() {
        let dir = scratch("delivery");
        let path = dir.join(socket::SOCKET);
        let engine = serve(&path, Identity::new(48_000, vec![]), Arc::new(Uneven)).unwrap();

        let mut stream = UnixStream::connect(&path).unwrap();
        writeln!(stream, "{HELLO}").unwrap();
        writeln!(
            stream,
            r#"{{"jsonrpc":"2.0","id":2,"method":"sub.subscribe","params":{{"topics":["queue"]}}}}"#
        )
        .unwrap();
        stream.flush().unwrap();

        let mut lines = BufReader::new(stream).lines();
        let _hello = lines.next().unwrap().unwrap();
        let _sub = lines.next().unwrap().unwrap();

        for _ in 0..200 {
            if engine.publish(&Event::new(Topic::Queue, serde_json::json!({"version":4}))) > 0 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }

        let event: serde_json::Value =
            serde_json::from_str(&lines.next().unwrap().unwrap()).unwrap();
        assert_eq!(event["method"], "event");
        assert!(event.get("id").is_none(), "an event is never correlated");
        assert_eq!(event["params"]["topic"], "queue");
        assert_eq!(event["params"]["version"], 4);

        drop(engine);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
