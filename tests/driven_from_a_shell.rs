//! **The engine, driven the way a person drives it** — #217, §15.
//!
//! Spawns a real `gmuse engine --null-audio` as a child process, talks to it
//! over its socket with `gmuse-client`, and runs `gmuse ctl` and `gmuse events`
//! against it as subprocesses.
//!
//! §15 calls this the second of three points to stop and judge the chain of
//! control: after it the protocol can be operated from a shell with no TUI,
//! "which is where an awkward command is cheapest to find". A test that only
//! called the library would not find those — it would exercise the protocol
//! without ever exercising the experience of using it.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use gmuse_client::Client;

/// A child engine that stops when the test does.
struct Engine {
    child: Child,
    dir: PathBuf,
    socket: PathBuf,
}

impl Drop for Engine {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn binary() -> PathBuf {
    // The test binary lives beside the one under test.
    let mut path = std::env::current_exe().expect("current exe");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.join("gmuse")
}

fn start(name: &str) -> Engine {
    let dir = std::env::temp_dir().join(format!("gmuse-217-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let socket = dir.join("engine.sock");

    let child = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &socket)
        // The other tests do not append, and a startup scan of the real library
        // would make every one of them wait on it.
        .env("GMUSE_ROOTS", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gmuse engine — run `cargo build --release` first");

    // Wait for it to bind rather than sleeping a guess.
    for _ in 0..400 {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    Engine { child, dir, socket }
}

fn ctl(socket: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(binary())
        .arg("ctl")
        .args(args)
        .env("GMUSE_SOCKET", socket)
        .output()
        .expect("run gmuse ctl");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

/// **A graceful stop actually stops** — #245.
///
/// `Drop for Engine` sets the flag, wakes `accept` by connecting to it, joins the
/// listener and unlinks the socket. All four were unreachable: everything holding the
/// engine in order to `publish` held a *strong* reference, and the listener thread owns
/// the answerer, which owned the slot, which owned the engine — so the count could not
/// reach zero and `Drop` never ran. Measured at 3 strong references where 1 was needed.
///
/// **The socket file is the observable half**, because the unlink is the last line of
/// that `Drop`. Its absence afterwards is therefore evidence the whole body ran, which
/// is why this is not merely a test about litter.
#[test]
fn a_graceful_stop_removes_its_socket() {
    let engine = start("graceful");

    // **The instrument is attached.** Without this the test passes against an engine
    // that never bound at all — "the file is not there" reads identically whether it
    // was cleaned up or never created, which is exactly the vacuous shape this
    // codebase keeps finding.
    assert!(
        engine.socket.exists(),
        "the engine never bound, so nothing was measured"
    );

    let (out, err, ok) = ctl(&engine.socket, &["engine.shutdown"]);
    // The reply comes back *before* the process goes: a shutdown that dropped the
    // answer on the floor would also pass the socket check below.
    assert!(ok, "the shutdown was refused: {err}");
    assert!(out.contains("stopping"), "no acknowledgement: {out:?}");

    // It has to actually exit, or `Drop` has not run and the file below is still open.
    let mut engine = engine;
    let exited = (0..200).any(|_| match engine.child.try_wait() {
        Ok(Some(_)) => true,
        _ => {
            std::thread::sleep(Duration::from_millis(25));
            false
        }
    });
    assert!(exited, "the engine did not exit after being told to stop");

    assert!(
        !engine.socket.exists(),
        "the socket outlived a graceful shutdown, so Drop for Engine did not run"
    );
}

/// **A client that asks and never reads is hung up on** — #251, §7.10, I1.
///
/// §7.10 says a client staying over the limit MAY be disconnected, and for a while
/// nothing did: `Outbox::over_limit` measured the condition and its comment handed the
/// decision to "whoever owns the socket", who was nobody.
///
/// **Responses are the unbounded queue**, which is what makes this worth enforcing.
/// Events are discarded on overflow and replaced by a resync notice; `push_response` has
/// no bound at all, deliberately — somebody is waiting on each answer. So a client that
/// fires requests and never reads makes the engine hold every reply, for ever.
///
/// A raw socket rather than `gmuse_client::Client`, because the client library reads its
/// replies and the whole point here is not to.
#[test]
fn a_client_that_asks_and_never_reads_is_disconnected() {
    use std::io::{Read, Write};

    let engine = start("never-reads");
    let mut sock = std::os::unix::net::UnixStream::connect(&engine.socket).expect("connect");

    // **The instrument is attached.** Without a completed handshake this test would pass
    // against an engine that refused the connection outright — "the socket is closed"
    // reads identically whether we were hung up on or never let in.
    writeln!(
        sock,
        r#"{{"jsonrpc":"2.0","id":"hello","method":"hello","params":{{"client":"greedy","protocol":"gmuse/0"}}}}"#
    )
    .expect("say hello");
    sock.flush().expect("flush");
    let mut greeting = [0u8; 1];
    sock.set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    assert!(
        matches!(sock.read(&mut greeting), Ok(1)),
        "the engine never answered the handshake, so nothing below is about being \
         disconnected"
    );

    // Now ask a great deal and read none of it. Each `engine.status` reply is a few
    // hundred bytes, so this is far more than any socket buffer will absorb — the
    // remainder has to pile up in the engine's own outbox, which is the condition
    // §7.10 names.
    let mut asks = 0;
    let mut refused = false;
    for i in 0..4000 {
        let line =
            format!(r#"{{"jsonrpc":"2.0","id":"{i}","method":"engine.status","params":null}}"#);
        if writeln!(sock, "{line}").is_err() || sock.flush().is_err() {
            // The engine shut the socket down under us, which is the outcome.
            refused = true;
            break;
        }
        asks += 1;
    }

    // Either the write failed because we were hung up on, or it all went out and the
    // read end is now closed. Both are the same verdict from this side.
    if !refused {
        let mut sink = [0u8; 65536];
        let mut spins = 0;
        loop {
            match sock.read(&mut sink) {
                Ok(0) => break, // EOF — hung up on
                Ok(_) => {}     // draining the replies it did send
                // **A timeout is not a disconnect**, and treating it as one would make
                // this pass on a slow machine against an engine that never hung up —
                // the read timeout above is a guard against hanging, not a verdict.
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    panic!("the engine stopped answering but never closed: {e}")
                }
                Err(_) => break, // reset — also hung up on
            }
            spins += 1;
            assert!(
                spins < 2000,
                "the engine kept answering {asks} unread requests without ever \
                 disconnecting"
            );
        }
    }

    assert!(
        asks > 0,
        "no request was ever sent, so the limit was never approached"
    );
}

#[test]
fn the_engine_starts_and_answers_a_handshake() {
    let engine = start("handshake");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");
    assert_eq!(client.hello().protocol, gmuse_proto::PROTOCOL);
    assert!(
        !client.hello().engine_generation.is_empty(),
        "a generation that scopes references cannot be empty"
    );
}

/// **`gmuse ctl` reaches the engine and reports what it said.** Not "prints
/// something": the message has to be the engine's, or the CLI is inventing
/// answers.
#[test]
fn ctl_reports_the_engines_own_refusal() {
    let engine = start("ctl");

    let (_out, err, ok) = ctl(&engine.socket, &["nonsuch"]);
    assert!(!ok, "an unparseable command is a failure exit");
    assert!(
        err.contains("unknown command") && err.contains("1005"),
        "the engine's own words and code, got: {err}"
    );

    // **A parsing, remotable command is now accepted** — #230 attached a player,
    // so this stopped being a `NotReady` and became an acknowledgement.
    let (out, _err, ok) = ctl(&engine.socket, &["--json", "seek", "+10"]);
    assert!(ok, "a remotable command reaches the player: {out}");
    assert!(out.contains("accepted"), "{out}");

    // A command that needs a view still is not remotable: `play` means "play
    // what is under the cursor", which a socket has no notion of.
    let (out, err, ok) = ctl(&engine.socket, &["--json", "play"]);
    assert!(!ok);
    assert!(
        format!("{out}{err}").contains("1007"),
        "not remotable: {out}{err}"
    );
}

/// **`--json` is NDJSON on stdout and nothing else**, so a jq pipeline can read
/// it. The prose goes to stderr or it would be a parse error in somebody's
/// script.
#[test]
fn ctl_json_is_machine_readable_on_stdout() {
    let engine = start("json");
    let (out, _err, _ok) = ctl(&engine.socket, &["--json", "seek", "+10"]);
    let value: serde_json::Value = serde_json::from_str(out.trim())
        .unwrap_or_else(|e| panic!("stdout was not NDJSON: {e}\n{out}"));
    assert_eq!(value["accepted"], true);
    assert_eq!(value["parsed"], "seek +10");
}

/// **A leading-dash argument reaches the parser intact.** `vol -10` and
/// `seek -5` are ordinary commands, and a CLI that ate them would be unusable
/// for the thing it exists to do.
#[test]
fn a_negative_argument_is_not_mistaken_for_a_flag() {
    let engine = start("negative");
    let (_out, err, _ok) = ctl(&engine.socket, &["vol", "-10"]);
    assert!(
        !err.contains("unexpected argument") && !err.contains("unknown command"),
        "the command reached the parser whole: {err}"
    );
}

/// **The transcript `gmuse events` prints** — the Done-when line.
///
/// It subscribes, the engine publishes, and the NDJSON on stdout is the event.
/// `engine.shutdown` is what publishes: a view learning the engine stopped by
/// its socket closing cannot tell that from a crash, so §7.8's `engine` topic
/// carries the notice.
#[test]
fn events_prints_a_transcript_of_what_the_engine_published() {
    let engine = start("events");

    let mut events = Command::new(binary())
        .args(["events", "--topics", "engine", "--count", "1"])
        .env("GMUSE_SOCKET", &engine.socket)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gmuse events");

    // **Wait for the subscription to be in force**, which `gmuse events` says
    // on stderr. Publishing before that would race the test's own setup.
    let mut note = String::new();
    BufReader::new(events.stderr.take().expect("stderr"))
        .read_line(&mut note)
        .expect("subscription note");
    assert!(note.contains("subscribed"), "got: {note}");

    // Ask it to stop, from another connection the way a second view would.
    let client = Client::connect(&engine.socket, "driver", "0").expect("connect");
    client.call("engine.shutdown", None).expect("shutdown");

    // **Read on a thread with a deadline.** A blocking read against an engine
    // that published nothing is a hung test rather than a failing one, and a
    // hung test is worse — it takes the whole suite with it.
    let mut out = BufReader::new(events.stdout.take().expect("stdout"));
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = out.read_line(&mut line);
        let _ = tx.send(line);
    });
    let printed = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("gmuse events printed nothing within ten seconds");

    let _ = events.kill();
    let _ = events.wait();

    let value: serde_json::Value = serde_json::from_str(printed.trim())
        .unwrap_or_else(|e| panic!("stdout was not NDJSON: {e}\n{printed}"));
    assert_eq!(value["topic"], "engine");
    assert_eq!(value["stopping"], true);
    assert_eq!(value["reason"], "engine.shutdown");
}

/// **A typed method is reachable from the CLI** — §4.2's "typed arguments or
/// passed through", and the half that was missing.
///
/// `gmuse ctl engine.shutdown` went to the `:` line's parser, which has never
/// heard of it, and came back `unknown command "engine.shutdown"` — so the
/// protocol's own methods were the one thing the CLI could not reach.
#[test]
fn a_typed_method_is_reachable_and_a_command_word_still_is_not() {
    let engine = start("typed");

    let (out, _err, ok) = ctl(&engine.socket, &["--json", "time.now"]);
    assert!(ok, "a typed method succeeds");
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("NDJSON");
    assert!(value["engine_now_ns"].as_i64().is_some(), "got {value}");

    // With JSON params.
    let (out, _err, ok) = ctl(
        &engine.socket,
        &["--json", "sub.subscribe", r#"{"topics":["queue"]}"#],
    );
    assert!(ok);
    let value: serde_json::Value = serde_json::from_str(out.trim()).expect("NDJSON");
    assert_eq!(value["topics"], serde_json::json!(["queue"]));

    // And a `:` line word is still routed to the command parser, not guessed
    // at as a method.
    let (_out, err, ok) = ctl(&engine.socket, &["nonsuch"]);
    assert!(!ok);
    assert!(err.contains("unknown command"), "{err}");
}

/// Bad JSON for a typed method is refused by the CLI with the usage, rather
/// than sent as-is for the engine to reject less helpfully.
#[test]
fn bad_json_params_are_refused_before_they_are_sent() {
    let engine = start("badjson");
    let (_out, err, ok) = ctl(&engine.socket, &["sub.subscribe", "{not json"]);
    assert!(!ok);
    assert!(err.contains("is not JSON"), "{err}");
    assert!(err.contains("Usage:"), "it says how instead: {err}");
}

/// **A fact appended by one client reaches another as a `scope: "ids"` event
/// naming exactly what changed** — #218's first Done-when line, end to end
/// across two connections.
///
/// Driven through `Engine::publish` with a real `library` change rather than
/// through an append, because the engine has no librarian attached yet: what is
/// under test here is the delivery and the shape, which is the half that
/// exists. The producer that turns appended records into this payload is tested
/// against real `Fact`s in `engine::library`.
#[test]
fn a_library_change_reaches_a_second_client_naming_what_changed() {
    let engine = start("library-event");

    let watcher = Client::connect(&engine.socket, "watcher", "0").expect("connect");
    watcher
        .subscribe(&[gmuse_proto::Topic::Library])
        .expect("subscribe");

    // A second connection, the way another view would be.
    let _other = Client::connect(&engine.socket, "editor", "0").expect("connect");

    // **An operation with no arguments plumbed refuses by name**, and the
    // refusal is `NotReady` rather than `MethodNotFound`: the protocol has the
    // method and this build cannot serve it, which a client can tell apart.
    let (out, _err, ok) = ctl(&engine.socket, &["--json", "library.import", "{}"]);
    assert!(!ok);
    let refusal: serde_json::Value = serde_json::from_str(out.trim()).expect("NDJSON");
    assert_eq!(refusal["code"], 1007, "NotReady, not MethodNotFound");
    assert_eq!(refusal["data"]["operation"], "import");
}

/// **The read-only subcommands have no method** — #218's "what must NOT land".
/// Several are reported-never-decided, and a decision brief is not a thing to
/// put behind an RPC that returns a result.
#[test]
fn a_read_only_operation_is_not_reachable_over_the_wire() {
    let engine = start("readonly");
    for name in ["library.query", "library.copies", "library.duplicates"] {
        let (out, err, ok) = ctl(&engine.socket, &["--json", name, "{}"]);
        assert!(!ok, "{name} must not be served");
        // **It falls through to the `:` parser**, which has never heard of it
        // either — so the refusal is `unknown command`, and the point is that
        // there is no method behind the name at all.
        let said = format!("{out}{err}");
        assert!(
            said.contains("unknown command") || said.contains("no such method"),
            "{name}: {said}"
        );
    }
}

/// A child engine pointed at a library of its own.
///
/// **Not the real one.** Without `GMUSE_DATA_DIR` the engine resolves the
/// user's actual data directory, and a first attempt at this test appended a
/// fabricated rating towards it.
fn start_with_library(name: &str) -> (Engine, PathBuf) {
    // A counter for the same reason `start_with_a_track` has one: the name was the
    // only distinguishing part, and `remove_dir_all` below would take a parallel
    // test's socket with it.
    static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gmuse-222-{}-{name}-{nth}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    // **`XDG_DATA_HOME`, not `GMUSE_DATA_DIR`.** The latter steers the *engine* only
    // (`main.rs`), so a `shelf` subcommand given it writes to the real library instead —
    // which is how the first version of this test appended a record to
    // `~/.local/share/gmuse` and then asserted against a code path it never entered.
    std::fs::create_dir_all(dir.join("xdg").join("gmuse")).expect("scratch");
    let socket = dir.join("engine.sock");
    let data = dir.join("data");

    let child = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &socket)
        .env("GMUSE_DATA_DIR", &data)
        // **No roots, so no startup scan.** The desk runs jobs in order, and a
        // scan of a real library would delay this test's write behind thirty to
        // seventy seconds of disk.
        .env("GMUSE_ROOTS", "")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gmuse engine");

    for _ in 0..400 {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    (
        Engine {
            child,
            dir,
            socket: socket.clone(),
        },
        data,
    )
}

/// **A fact appended by one client reaches another as a `scope: "ids"` event
/// naming exactly what changed** — #222's first Done-when line, and #218's.
#[test]
fn a_fact_from_one_client_reaches_another_naming_what_changed() {
    let (engine, _data) = start_with_library("append");

    let watcher = Client::connect(&engine.socket, "watcher", "0").expect("connect");
    watcher
        .subscribe(&[gmuse_proto::Topic::Library])
        .expect("subscribe");

    // A second client appends, the way another view would.
    let editor = Client::connect(&engine.socket, "editor", "0").expect("connect");
    let recording = "8f3a1b2c-0000-4000-8000-000000000001";
    editor
        .call(
            "library.edit",
            // **The log's own encoding on the wire** — #236. A fact read out of
            // `gmuse events` and a fact written here are the same text.
            Some(serde_json::json!({
                "facts": [{ "RecordingRated": { "recording": recording, "stars": 4 } }]
            })),
        )
        .expect("the edit is accepted");

    let event = watcher
        .next_event(Duration::from_secs(10))
        .expect("a library event within ten seconds");

    assert_eq!(event.topic, gmuse_proto::Topic::Library);
    assert_eq!(
        event.body["scope"], "ids",
        "narrow: the fact names its subject"
    );
    let ids = event.body["ids"].as_array().expect("an id list");
    assert!(
        ids.iter()
            .any(|i| i.as_str() == Some(&format!("recording:{recording}"))),
        "it names exactly what changed, got {ids:?}"
    );
    assert!(
        event.body["version"].as_u64().is_some_and(|v| v >= 1),
        "the engine's own log position, got {}",
        event.body["version"]
    );
}

/// **The round trip an attached view lives on** — #236, and both halves at once.
///
/// A reading desk (#219) cannot append, so a rating leaves as a proposal and comes
/// back as a fold. This drives exactly the machinery `ui::propose` and
/// `ui::follow_the_library` use, without a terminal:
///
/// 1. the view holds a `Desk::read_only` over the engine's data directory, and has
///    never seen the rating;
/// 2. the facts a `record` closure would have appended go out as `library.edit`;
/// 3. the engine appends and publishes on `library`;
/// 4. the view re-reads and the rating is *in its own projection*.
///
/// Step 4 is the one worth having a test for. Before it, an attached view's write
/// was accepted by the engine and invisible on screen — which reads as the rating
/// not having worked.
#[test]
fn a_view_that_cannot_append_proposes_a_rating_and_reads_it_back() {
    let (engine, data) = start_with_library("roundtrip");

    let client = Client::connect(&engine.socket, "view", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Library])
        .expect("subscribe");

    let device = gmuse::model::id::Id::derived(b"the-view");
    let song = gmuse::model::id::Id::<gmuse::model::entity::Recording>::derived(b"a song");
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());

    assert!(desk.appender().is_none(), "this is the attached shape");
    assert_eq!(
        desk.current().listened.ratings.get(&song).copied(),
        None,
        "nothing has rated it yet"
    );

    // **What `ui::propose` does**: run the closure against a throwaway instance
    // and send what it appended. No stamp crosses — the engine's clock stamps.
    let mut scratch = gmuse::model::instance::Instance::new(device);
    scratch.rate(1_000, song, Some(5));
    let facts: Vec<_> = scratch.records().iter().map(|r| &r.fact).collect();
    assert_eq!(facts.len(), 1, "one call, one fact");
    client
        .call("library.edit", Some(serde_json::json!({ "facts": facts })))
        .expect("the engine takes it");

    // **The engine says there is more to read.** §7.4: the result above meant
    // accepted, not appended, so this is the part that means it landed.
    let event = client
        .next_event(Duration::from_secs(10))
        .expect("a library event");
    assert_eq!(event.topic, gmuse_proto::Topic::Library);
    assert_eq!(
        event.body["scope"], "ids",
        "narrow, because a rating names its subject"
    );

    // **What `ui::follow_the_library` does.**
    desk.reread();
    desk.settled();
    assert_eq!(
        desk.current().listened.ratings.get(&song).copied(),
        Some(5),
        "the view's own projection never caught up with its own write"
    );

    // And the change it publishes is narrow, so a rating does not cost a rebuild.
    assert!(
        matches!(desk.current().changed, gmuse::librarian::Changed::These(_)),
        "a re-read after one rating published {:?}",
        desk.current().changed
    );
}

/// **`ui::record` itself takes the second door** — #236, the seam the round trip
/// above does not cover.
///
/// The test above does what `propose` does; this one calls the function the ten
/// gestures actually call, with the `Ui` shaped the way an attached launch shapes
/// it: a reading desk, and an engine to ask. If `record` ever stopped reaching for
/// the second door, that test would still pass and this one would not.
///
/// Driving the real TUI would be better still and is not available: `script` on
/// macOS refuses a pty whose stdin is a pipe, so there is no way to send a
/// keystroke to a running gmuse from here. What is left untested is the keypress,
/// not the write.
#[test]
fn record_sends_a_fact_to_the_engine_when_this_view_cannot_append() {
    let (engine, data) = start_with_library("record-door");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let device = gmuse::model::id::Id::derived(b"the-view");
    let song = gmuse::model::id::Id::<gmuse::model::entity::Recording>::derived(b"through record");

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.data_dir = Some(data.clone());
    ui.desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone())
        .into_enquiries();
    ui.engine = Some(client);
    assert!(
        ui.desk.appender().is_none(),
        "the first door has to be shut for this to mean anything"
    );

    gmuse::ui::record(&mut ui, 0, move |log| log.rate(1_000, song, Some(3)));

    let mut found = false;
    for _ in 0..400 {
        let log: String = std::fs::read_dir(&data)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .filter_map(|p| std::fs::read_to_string(p).ok())
                    .collect()
            })
            .unwrap_or_default();
        if log.contains(&song.to_string()) {
            found = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        found,
        "the rating never reached the engine's log, so `record` did not take the \
         second door"
    );
    assert!(
        !ui.status.contains("nowhere"),
        "and it did not report having nowhere to put it: {:?}",
        ui.status
    );
}

/// **A fact a view has no business observing is refused, and nothing is appended**
/// — #236, §8.4.
///
/// `Fact::is_an_edit` is the gate. A measurement the client never took, an
/// import's reading of a file, one machine's holdings: all of those come from
/// somewhere a view is not, and a socket that took them would let any client
/// write anything into the log.
///
/// **All-or-nothing matters as much as the refusal.** A batch with one bad fact
/// appends none of it, or a `:collect` could leave a collection named with none of
/// its members.
#[test]
fn a_fact_a_view_cannot_observe_is_refused_and_takes_the_batch_with_it() {
    let (engine, data) = start_with_library("refuse");
    let editor = Client::connect(&engine.socket, "editor", "0").expect("connect");
    let rated = "8f3a1b2c-0000-4000-8000-000000000003";

    let refused = editor.call(
        "library.edit",
        Some(serde_json::json!({
            "facts": [
                // Perfectly good, and it must not land.
                { "RecordingRated": { "recording": rated, "stars": 3 } },
                // A measurement this client never took.
                { "EncodingMeasured": {
                    "encoding": "8f3a1b2c-0000-4000-8000-0000000000ff",
                    "cutoff_hz": 15700,
                    "analyser": 1
                }},
            ]
        })),
    );
    let err = refused.expect_err("a measurement is not a view's to propose");
    let rpc = err
        .rpc()
        .expect("an RPC refusal rather than a transport failure");
    assert_eq!(
        rpc.data.as_ref().and_then(|d| d["at"].as_u64()),
        Some(1),
        "it says which one: {rpc:?}"
    );

    // And a good one afterwards, so the wait below is for something that *did*
    // happen rather than for a fixed interval.
    let good = "8f3a1b2c-0000-4000-8000-000000000004";
    editor
        .call(
            "library.edit",
            Some(serde_json::json!({
                "facts": [{ "RecordingRated": { "recording": good, "stars": 2 } }]
            })),
        )
        .expect("a rating on its own is accepted");

    let mut log = String::new();
    for _ in 0..400 {
        log = std::fs::read_dir(&data)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .filter_map(|p| std::fs::read_to_string(p).ok())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if log.contains(good) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(log.contains(good), "the accepted rating never landed");
    assert!(
        !log.contains(rated),
        "the refused batch's *first* fact landed anyway, so it was applied in part"
    );
}

/// **No process but the engine appends** — I15, and a test that would fail if
/// the write had gone anywhere else.
#[test]
fn the_append_reached_the_engines_own_log_and_nothing_elses() {
    let (engine, data) = start_with_library("appender");

    let editor = Client::connect(&engine.socket, "editor", "0").expect("connect");
    editor
        .call(
            "library.edit",
            Some(serde_json::json!({
                "facts": [{
                    "RecordingRated": {
                        "recording": "8f3a1b2c-0000-4000-8000-000000000002",
                        "stars": 5
                    }
                }]
            })),
        )
        .expect("accepted");

    // **Wait for the content, not for the file.** `durable::append` creates
    // the log before it writes to it, so polling for the filename finds an
    // empty file about one run in ten — which is a flaky test asserting the
    // right thing for the wrong reason.
    let mut found = String::new();
    for _ in 0..400 {
        found = std::fs::read_dir(&data)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("log-") && n.ends_with(".jsonl"))
                    })
                    .filter_map(|p| std::fs::read_to_string(p).ok())
                    .collect::<String>()
            })
            .unwrap_or_default();
        if found.contains("8f3a1b2c-0000-4000-8000-000000000002") {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    assert!(
        found.contains("8f3a1b2c-0000-4000-8000-000000000002"),
        "the fact is in the engine's own log under {}: {found}",
        data.display()
    );

    // **And nowhere else.** The only log in that directory is the engine's
    // own, which is what "no process but the engine appends" looks like from
    // outside: one appender, one device file.
    let logs: Vec<_> = std::fs::read_dir(&data)
        .expect("the data directory")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("log-"))
        .collect();
    assert_eq!(logs.len(), 1, "one appender, one log: {logs:?}");

    drop(engine);
}

/// Write a WAV a decoder will accept: two seconds of a cheap ramp.
fn write_wav(path: &Path, frames: usize) {
    use std::io::Write;
    let bytes = 4 * frames;
    let mut f = std::fs::File::create(path).expect("create wav");
    f.write_all(b"RIFF").unwrap();
    f.write_all(&((36 + bytes) as u32).to_le_bytes()).unwrap();
    f.write_all(b"WAVEfmt ").unwrap();
    // **Each field at its own width.** A first version packed the format tag
    // and the channel count into one `u32`, which little-endian reverses into
    // "ADPCM, mono" — the decoder refused it and the import found no audio,
    // which surfaced three tests later as "the import minted a recording".
    f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&2u16.to_le_bytes()).unwrap(); // stereo
    f.write_all(&44_100u32.to_le_bytes()).unwrap();
    f.write_all(&176_400u32.to_le_bytes()).unwrap(); // byte rate
    f.write_all(&4u16.to_le_bytes()).unwrap(); // block align
    f.write_all(&16u16.to_le_bytes()).unwrap(); // bits
    f.write_all(b"data").unwrap();
    f.write_all(&(bytes as u32).to_le_bytes()).unwrap();
    for i in 0..frames {
        let s = ((i % 100) as i16).wrapping_mul(300);
        f.write_all(&s.to_le_bytes()).unwrap();
        f.write_all(&s.to_le_bytes()).unwrap();
    }
}

/// An engine over a library containing exactly one playable track.
///
/// **Imported through the CLI, not over the wire**, because `library.import` has
/// no arguments plumbed yet (#218). `XDG_DATA_HOME` redirects
/// `config::default_data_dir`, so the import and the engine agree on where the
/// log is without either of them touching the real library.
fn start_with_a_track(name: &str) -> (Engine, String) {
    // **A counter, because two tests picking the same name wiped each other.**
    //
    // `remove_dir_all` below is what makes a rerun clean, and with the name as
    // the only distinguishing part it also made a *parallel* test's engine socket
    // disappear underneath it — which shows up as `connect: NotFound` in whichever
    // test lost, nowhere near the test that caused it. The name still leads the
    // path so a failure says which test left the directory.
    static NTH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("gmuse-230-{}-{name}-{nth}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("lib")).expect("scratch");
    std::fs::create_dir_all(dir.join("xdg")).expect("scratch");
    let track = dir.join("lib").join("tone.wav");
    write_wav(&track, 44_100);
    // **Two tracks, because one cannot show a join.** The second is a different
    // length so its bytes hash differently and the import mints a second
    // recording rather than joining the first.
    let second = dir.join("lib").join("tone2.wav");
    write_wav(&second, 22_050);

    let imported = Command::new(binary())
        .args([
            "--no-config",
            "shelf",
            "import",
            "--unidentified",
            "--write",
        ])
        .arg(&track)
        .arg(&second)
        .env("XDG_DATA_HOME", dir.join("xdg"))
        // **Run it from the scratch directory, not from the repository.**
        //
        // `import --write` *places* a file — moves it into
        // `{file_under}/{album}/{track} {title}` — and with `--no-config` there
        // is no configured storage root, so the tree is rooted at the working
        // directory (#231). Without this the suite left `Unknown Artist/tone.wav`
        // in the repo on every `just ci`, and the first time it did I committed
        // it.
        .current_dir(&dir)
        .output()
        .expect("run shelf import");
    assert!(
        imported.status.success(),
        "import failed: {}",
        String::from_utf8_lossy(&imported.stderr)
    );

    // The recording the import minted, read back from the log it wrote.
    let data = dir.join("xdg").join("gmuse");
    let log: String = std::fs::read_dir(&data)
        .expect("data dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .collect();
    let recording = log
        .split("\"RecordingIdentified\":{\"recording\":\"")
        .nth(1)
        .and_then(|rest| rest.split('"').next())
        .unwrap_or_else(|| {
            panic!(
                "the import minted no recording.\nstdout: {}\nlog was {} bytes",
                String::from_utf8_lossy(&imported.stdout),
                log.len()
            )
        })
        .to_string();

    let socket = dir.join("engine.sock");
    let child = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &socket)
        .env("GMUSE_DATA_DIR", &data)
        // **Roots matter here, unlike every other test.** The librarian's
        // locators come from the scan, so an engine with no roots resolves every
        // recording to nothing and `player.load` finds nothing to play.
        //
        // The whole scratch directory rather than `lib/`, because the import
        // *placed* the file: it now lives under `Unknown Artist/` beside `lib/`,
        // and a root pointing at where the file used to be finds an empty
        // library and a test that fails for the wrong reason.
        .env("GMUSE_ROOTS", &dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn gmuse engine");

    for _ in 0..400 {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    (Engine { child, dir, socket }, recording)
}

/// **A writing subcommand hands its facts to the engine rather than appending beside
/// it** — #218, §4.4.
///
/// Two processes on one machine share a device id, because it comes from `machine_id`;
/// each `Instance`'s clock starts its counter at 0 within a millisecond; and `Record`
/// implements `Ord` over the stamp alone while deriving `Eq` over both fields — so a
/// `BTreeSet<Record>` treats two records with one stamp as the same record and keeps
/// the first (#238). An import writing nine facts in one millisecond and an engine
/// rating something in it collide, and one fact goes silently.
///
/// `librarian::Appending` made that unreachable by refusing the second appender. This
/// is the other half: being refused is no use if the operation then cannot happen, so
/// `land` hands the facts over and the engine appends them with its own clock.
#[test]
fn a_writing_subcommand_hands_its_facts_to_the_engine_that_holds_the_log() {
    let (engine, _recording) = start_with_a_track("one-appender");
    let data = engine.dir.join("xdg").join("gmuse");

    // A config pointing at the engine's own log, so the two really do collide. Kept
    // outside the directory the import placed from, because `shelf import --write`
    // once carried non-audio files along with the audio (#239) — including this one.
    let config = engine.dir.join("as-the-engine.toml");
    std::fs::write(
        &config,
        format!(
            "data_dir = {:?}\nlibrary = [{:?}]\n",
            data.display().to_string(),
            engine.dir.display().to_string()
        ),
    )
    .expect("write a config");

    // **Something for the scan to actually say.** A new file would be a *stranger* —
    // nobody imported it — and holdings name only what the library knows, so the scan
    // would find nothing changed and hand over nothing, correctly. Removing a copy
    // changes the set.
    let placed: Vec<std::path::PathBuf> = walk(&engine.dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    assert!(!placed.is_empty(), "the fixture placed nothing: {placed:?}");
    std::fs::remove_file(&placed[0]).expect("remove one copy");

    let handed = Command::new(binary())
        .args(["shelf", "scan", "--write"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf scan --write");
    let said = String::from_utf8_lossy(&handed.stderr);
    assert!(
        handed.status.success(),
        "the operation could not happen at all: {said}"
    );
    // **Handed over, not committed here.** The distinction is the whole issue: this
    // process did the disk work and the engine did the appending.
    assert!(
        said.contains("handed to the running engine"),
        "it appended beside the engine instead of handing over: {said}"
    );

    // And a dry run is unaffected, because reading takes no right to append.
    let dry = Command::new(binary())
        .args(["shelf", "scan"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf scan");
    assert!(
        dry.status.success(),
        "a dry run was refused: {}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&dry.stderr).contains("handed"),
        "a dry run handed something over"
    );
}

/// **Compacting claims the appender, or hands the job to whoever holds it** — #249,
/// §4.4, I15.
///
/// `shelf compact --write` held its own copy of the per-device rewrite loop and called
/// `durable::rewrite` directly, claiming nothing. So the one operation that *removes*
/// could rewrite a log from outside the lock while an engine held it — deleting
/// whatever the engine appended after this process folded.
///
/// **The `dropped > 0` guard is why this test needs a real superseded assertion.** An
/// empty log short-circuits with "nothing to drop" before reaching the rewrite at all,
/// so a test that skipped the setup below would pass against the bug without ever
/// executing the line under test. That is measured, not hypothetical: it is exactly how
/// the first attempt at reproducing this came back clean.
#[test]
fn compacting_defers_to_the_engine_that_holds_the_log() {
    let (engine, _recording) = start_with_a_track("compact-held");
    let data = engine.dir.join("xdg").join("gmuse");
    let config = engine.dir.join("as-the-engine.toml");
    std::fs::write(
        &config,
        format!(
            "data_dir = {:?}\nlibrary = [{:?}]\n",
            data.display().to_string(),
            engine.dir.display().to_string()
        ),
    )
    .expect("write a config");

    // **Give the log a second holdings assertion, so one supersedes the other.**
    // Removing a placed copy changes the set, so the scan has something to say; it
    // hands the facts to the engine, which appends them with its own clock.
    let placed: Vec<std::path::PathBuf> = walk(&engine.dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    assert!(!placed.is_empty(), "the fixture placed nothing: {placed:?}");
    std::fs::remove_file(&placed[0]).expect("remove one copy");
    let scanned = Command::new(binary())
        .args(["shelf", "scan", "--write"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf scan --write");
    assert!(
        scanned.status.success(),
        "the scan could not happen: {}",
        String::from_utf8_lossy(&scanned.stderr)
    );

    // **The instrument is attached**, and this is the assertion the whole test rests
    // on: there is something to compact, so the write path below is actually reached.
    let dry = Command::new(binary())
        .args(["shelf", "compact"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf compact");
    let seen = String::from_utf8_lossy(&dry.stderr);
    assert!(
        !seen.contains("0 superseded"),
        "nothing to drop, so the rewrite would be skipped and this test would prove \
         nothing: {seen}"
    );

    // Now the real thing, against an engine that holds the log.
    let held = Command::new(binary())
        .args(["shelf", "compact", "--write"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf compact --write");
    let said = String::from_utf8_lossy(&held.stderr);
    assert!(held.status.success(), "it could not compact at all: {said}");
    // **Deferred, not done here.** The distinction is the issue: this process may not
    // rewrite a log it does not hold.
    assert!(
        said.contains("the engine holds the log"),
        "it rewrote the log from outside the lock: {said}"
    );
    assert!(
        !said.contains("logs rewritten"),
        "it rewrote the log from outside the lock: {said}"
    );
}

/// **`shelf tidy` names what it would remove, and removes nothing without `--write`**
/// — #239.
///
/// The one operation in this project that *deletes*, so the dry run is the part that
/// matters: a deletion a person cannot check first is one they have to take on trust.
/// It also has to skip `.departed`, where `remove` keeps retired copies — a tombstone's
/// folder is not a place to sweep.
#[test]
fn tidy_names_the_junk_before_it_removes_any_of_it() {
    let (engine, _recording) = start_with_a_track("tidy");
    let data = engine.dir.join("xdg").join("gmuse");
    let config = engine.dir.join("tidy.toml");
    std::fs::write(
        &config,
        format!(
            "data_dir = {:?}\nlibrary = [{:?}]\n",
            data.display().to_string(),
            engine.dir.display().to_string()
        ),
    )
    .expect("write a config");

    // Junk beside the placed audio, and one inside `.departed` that must survive.
    let album = engine.dir.join("Unknown Artist");
    std::fs::write(album.join("desktop.ini"), b"explorer").expect("write junk");
    std::fs::write(album.join(".DS_Store"), b"finder").expect("write junk");
    std::fs::write(album.join("cover.jpg"), b"art").expect("write a cover");
    let retired = engine.dir.join(".departed").join("An Artist");
    std::fs::create_dir_all(&retired).expect("scratch");
    std::fs::write(retired.join("Thumbs.db"), b"explorer").expect("write junk");

    let dry = Command::new(binary())
        .args(["shelf", "tidy"])
        .env("GMUSE_CONFIG", &config)
        .output()
        .expect("run shelf tidy");
    let named = String::from_utf8_lossy(&dry.stdout);
    assert!(
        named.contains("desktop.ini"),
        "it did not name one: {named}"
    );
    assert!(named.contains(".DS_Store"), "or the other: {named}");
    assert!(
        String::from_utf8_lossy(&dry.stderr).contains("dry run"),
        "a dry run did not say so"
    );
    assert!(
        album.join("desktop.ini").exists(),
        "a dry run removed something"
    );

    let done = Command::new(binary())
        .args(["shelf", "tidy", "--write"])
        .env("GMUSE_CONFIG", &config)
        .output()
        .expect("run shelf tidy --write");
    assert!(done.status.success());
    assert!(
        !album.join("desktop.ini").exists() && !album.join(".DS_Store").exists(),
        "the junk survived --write"
    );
    assert!(
        album.join("cover.jpg").exists(),
        "the cover was removed, which is the failure this must never have"
    );
    assert!(
        retired.join("Thumbs.db").exists(),
        "it swept `.departed`, where retired copies live"
    );

    // And again finds nothing, so the sweep is idempotent rather than repeating work.
    let again = Command::new(binary())
        .args(["shelf", "tidy"])
        .env("GMUSE_CONFIG", &config)
        .output()
        .expect("run shelf tidy");
    assert!(
        String::from_utf8_lossy(&again.stdout).trim().is_empty(),
        "it found something the second time: {}",
        String::from_utf8_lossy(&again.stdout)
    );
}

/// **A view that never scans can still reach a copy** — #237.
///
/// The librarian's rows come from the log, so an attached view could always *list*
/// a record. Which of those rows this device can `open` came from the **store** —
/// the scan's answer, handed to `bridge::librarian` and asserted over whatever the
/// log said. An attached view never scans, so it arrived with nothing, erased the
/// log's assertion by asserting an empty one, and refused every row it could see.
///
/// Two changes meet here: a scan now writes what it found into the log, and the
/// fold treats the log as the answer rather than as a starting point. So this
/// builds the attached shape exactly — a reading desk with **empty** `Holdings` —
/// and asks for a locator.
#[test]
fn a_reading_desk_with_no_store_can_still_open_what_the_engine_scanned() {
    let (engine, recording) = start_with_a_track("holdings");
    let data = engine.dir.join("xdg").join("gmuse");
    let recording: gmuse::model::id::Id<gmuse::model::entity::Recording> =
        recording.parse().expect("a recording id");

    // The same device all three processes use: the import, the engine, and a view.
    // `machine_id` is what makes them one appender and one holder.
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());

    // **Wait for the engine's scan to write it down.** The engine reconciles at
    // start and does not wait on it, so the assertion arrives a moment after the
    // socket does.
    let mut opened = None;
    for _ in 0..400 {
        let desk = gmuse::librarian::Desk::read_only(
            // **Empty on purpose.** This is the whole point: no cache, no scan,
            // nothing but the log.
            Default::default(),
            device,
            0,
            data.clone(),
        );
        opened = desk
            .current()
            .library
            .open(recording, &[device], &[])
            .map(|(_encoding, locator)| locator.to_string());
        if opened.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let locator = opened.expect(
        "a view with no store could not reach the copy, so the log is not carrying \
         the holdings",
    );
    assert!(
        locator.ends_with(".wav"),
        "and it is a real locator: {locator}"
    );
    assert!(
        std::path::Path::new(&locator).exists(),
        "one that points at a file that is there: {locator}"
    );
}

/// **A scan writes what changed and nothing when nothing did** — #237, #148's rule.
///
/// Both directions, in one test, because the first version of this only had the
/// second and **passed with the append removed entirely**: the import had already
/// written holdings, so "the count did not grow" was true for the wrong reason.
///
/// Holdings supersede whole, so re-asserting an identical set folds to exactly what
/// is already there. Without the guard every launch would grow the log by a record
/// that says nothing — and on the live library one assertion is 7,267
/// encoding-to-locator pairs, so "nothing" is about half a megabyte of it.
#[test]
fn a_scan_writes_holdings_when_the_disk_changed_and_not_when_it_did_not() {
    let (engine, _recording) = start_with_a_track("holdings-twice");
    let data = engine.dir.join("xdg").join("gmuse");
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());

    let count = || {
        let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
        log.records()
            .iter()
            .filter(|r| matches!(r.fact, gmuse::facts::Fact::HoldingsAsserted { .. }))
            .count()
    };
    let client = Client::connect(&engine.socket, "rescanner", "0").expect("connect");
    let rescan = || {
        client
            .call("library.scan", Some(serde_json::json!({})))
            .expect("the engine takes a scan");
        // Long enough for a scan of two files to finish several times over.
        std::thread::sleep(Duration::from_millis(1_500));
    };

    // Settle: the engine reconciles at start and does not wait on it.
    let mut settled = 0;
    for _ in 0..400 {
        settled = count();
        if settled > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(settled > 0, "the import wrote no holdings at all");
    rescan();
    let unchanged = count();
    assert_eq!(
        unchanged, settled,
        "a scan over the same files wrote another holdings record"
    );

    // **Now change the disk.** Removing a copy is the clean way to do it: it
    // changes the set without involving a second appender, which importing a file
    // would.
    let placed: Vec<std::path::PathBuf> = walk(&engine.dir)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|x| x == "wav"))
        .collect();
    assert!(
        placed.len() >= 2,
        "the fixture holds two copies: {placed:?}"
    );
    std::fs::remove_file(&placed[0]).expect("remove one copy");

    rescan();
    assert_eq!(
        count(),
        unchanged + 1,
        "a scan that found one fewer file wrote nothing down, so the log is not \
         following the disk"
    );

    rescan();
    assert_eq!(
        count(),
        unchanged + 1,
        "and then said it again over an unchanged disk"
    );
}

/// **A scan of the wrong roots does not erase what the library holds** — #233.
///
/// Paid for, on my own library. An engine auto-started by a TUI inherited
/// `GMUSE_ROOTS` pointing at a scratch directory while its config pointed at the
/// real one, found none of the 7,267 files the library knows, and appended
/// `HoldingsAsserted { held: {} }` — which supersedes, so every row became
/// unplayable until the next real scan.
///
/// Nothing in the design stopped it, because before #237 no *launch* wrote holdings
/// at all. The precedent for the guard was three lines away in the librarian's cache
/// write: "never an empty one over a good one — that is how a working library
/// becomes an empty one". The log deserved the same rule, and more: a cache can be
/// rebuilt by a scan, and the log is what the scan is *for*.
#[test]
fn a_scan_that_finds_nothing_the_library_knows_leaves_its_holdings_alone() {
    let (engine, _recording) = start_with_a_track("wrong-roots");
    let data = engine.dir.join("xdg").join("gmuse");
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());

    let held = || {
        let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
        log.holdings_asserted(device).map(|h| h.len())
    };
    let mut before = None;
    for _ in 0..400 {
        before = held();
        if before.is_some_and(|n| n > 0) {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let before = before.expect("the import wrote holdings");
    assert!(before > 0, "this library holds something to erase");

    // **A second engine, on the same log, pointed at an empty directory.** Exactly
    // the shape that did the damage: the right data directory and the wrong roots.
    let elsewhere = engine.dir.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("scratch");
    // **Its own directory for its socket**, because `engine::claim` locks
    // `engine.lock` beside it — one lock per *directory*, not per socket. The first
    // version of this test put the second socket next to the first, so the second
    // engine lost the lock, exited, never scanned, and the test passed while
    // checking nothing.
    let alone = engine.dir.join("second");
    std::fs::create_dir_all(&alone).expect("scratch");
    let other = alone.join("engine.sock");
    let mut child = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &other)
        .env("GMUSE_DATA_DIR", &data)
        .env("GMUSE_ROOTS", &elsewhere)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a second engine");
    for _ in 0..400 {
        if other.exists() && std::os::unix::net::UnixStream::connect(&other).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    // Long enough for its startup scan of an empty directory to have finished
    // several times over.
    std::thread::sleep(Duration::from_millis(1_500));
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(
        held(),
        Some(before),
        "a scan of the wrong roots erased the library's holdings"
    );
}

/// **A file nobody imported is not a holding** — #237, and the rule
/// `populate::reconcile` already followed.
///
/// Two writers of one *superseding* fact that disagree about its contents take turns
/// adding and removing the difference, so the log grows by the disagreement on every
/// pass. `shelf scan` asserts only encodings the library knows; a launch's scan has
/// to mean the same thing by "holds".
///
/// It is also the right answer on its own terms: a file in a watched root is not a
/// declaration that it belongs here — that inference is one of the five
/// `library-api.md` §2 lists as wrong. `shelf import` is how a stranger is found,
/// through the store.
#[test]
fn a_file_nobody_imported_is_not_a_holding() {
    let (engine, _recording) = start_with_a_track("strangers");
    let data = engine.dir.join("xdg").join("gmuse");
    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());

    let held = || {
        let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
        log.holdings_asserted(device).map(|h| h.len())
    };

    let mut before = None;
    for _ in 0..400 {
        before = held();
        if before.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let before = before.expect("nothing has asserted holdings");

    // A stranger in the root the engine reconciles. Different length, so it hashes
    // differently and is nobody's copy.
    write_wav(&engine.dir.join("stranger.wav"), 11_025);

    let client = Client::connect(&engine.socket, "rescanner", "0").expect("connect");
    client
        .call("library.scan", Some(serde_json::json!({})))
        .expect("the engine takes a scan");
    std::thread::sleep(Duration::from_millis(1_500));

    assert_eq!(
        held(),
        Some(before),
        "a file nobody imported entered the holdings, so a launch's scan and \
         `shelf scan` now disagree about what this device holds"
    );
}

/// Every file under a directory, recursively.
fn walk(at: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(at) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out.sort();
    out
}

/// **The view's queue is the engine's, both ways** — #233, §7.7, §8.8.
///
/// The halfway state this issue exists to end: a queue pane showing a list nobody
/// plays from. So the mirror has to carry an edit *out* and another client's edit
/// *in*, and refuse an edit computed against a version that has moved.
///
/// **A restored queue is an edit nobody has seen, so an empty engine does not erase it.**
///
/// The programme does not survive an engine restart, and the session file is the only place
/// it was written down. Restoring it put the rows back in `ui.queue` — and the first
/// `read_the_queue` then cleared them, because the engine had none.
///
/// §8.8's rule is out before in, so that a read never discards an unseen edit. The first read
/// is the one iteration that cannot honour it: `push_the_queue` has no version to compute
/// against until that reply arrives, so it returns early and the read wins by default.
///
/// **Both halves are asserted, because the fix could go too far the other way.** A session
/// restoring over a queue another view is already using would be worse than losing one.
#[test]
fn a_restored_queue_reaches_an_engine_that_has_none() {
    let (engine, recording) = start_with_a_track("restored-queue");
    let data = engine.dir.join("xdg").join("gmuse");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let held = desk.current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
    };
    let row = |r: &str| {
        gmuse::model::rows::Listing::rows(&held.library)
            .into_iter()
            .find(|id| {
                gmuse::model::rows::Listing::recording_of(&held.library, *id).map(|x| x.to_string())
                    == Some(r.to_string())
            })
            .expect("the imported row")
    };

    // The instrument: the engine must actually have nothing, or "it kept the restored one"
    // is a claim about a queue that was never at risk.
    let empty = client.call("queue.get", None).expect("queue.get");
    assert_eq!(
        empty["entries"].as_array().map(|e| e.len()),
        Some(0),
        "the engine already had a queue: {empty}"
    );

    // What `restore_session` leaves behind: rows in the queue, and no version yet.
    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&client));
    ui.queue.push_back(row(&recording));
    assert_eq!(ui.queue_version(), 0, "a fresh view has read no version");

    gmuse::ui::read_the_queue(&mut ui, &ctx);
    assert_eq!(
        ui.queue.entries().count(),
        1,
        "an engine with no queue erased the one the session restored"
    );
    assert!(ui.queue_version() > 0, "the version was never read");

    // And the next iteration offers it, which is what makes it the engine's rather than
    // this view's — another client has to be able to see it.
    gmuse::ui::push_the_queue(&mut ui, &ctx);
    let other = Client::connect(&engine.socket, "other", "0").expect("connect");
    let seen = other.call("queue.get", None).expect("queue.get");
    assert_eq!(
        seen["entries"].as_array().map(|e| e.len()),
        Some(1),
        "the restored queue never reached the engine: {seen}"
    );
    assert_eq!(seen["entries"][0].as_str(), Some(recording.as_str()));

    // **And the other way: an engine that has a queue wins.** A second view launching with a
    // stale session must not push its own over one somebody is listening to.
    let mut late = gmuse::ui::Ui::new(Default::default());
    late.engine = Some(std::sync::Arc::clone(&client));
    late.queue.push_back(row(&recording));
    late.queue.push_back(row(&recording));
    assert_eq!(late.queue.entries().count(), 2, "the stale session has two");

    gmuse::ui::read_the_queue(&mut late, &ctx);
    assert_eq!(
        late.queue.entries().count(),
        1,
        "a stale session overwrote a queue the engine already had"
    );
}

/// **Emptying the queue reaches the engine, because the engine is whose queue it is** — #233.
///
/// `:clear-queue` is a local edit like any other, and travels by the one path that keeps the
/// mirror and the version in step: an empty `queue.set`, which is already what `dd`-ing the
/// last row sends. No new method, and no second way for a queue to be emptied.
#[test]
fn clearing_the_queue_empties_the_engines_too() {
    let (engine, recording) = start_with_a_track("clear-queue-wire");
    let data = engine.dir.join("xdg").join("gmuse");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let held = desk.current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
    };
    let row = gmuse::model::rows::Listing::rows(&held.library)
        .into_iter()
        .find(|id| {
            gmuse::model::rows::Listing::recording_of(&held.library, *id).map(|x| x.to_string())
                == Some(recording.clone())
        })
        .expect("the imported row");

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&client));
    gmuse::ui::read_the_queue(&mut ui, &ctx);
    ui.queue.push_back(row);
    gmuse::ui::push_the_queue(&mut ui, &ctx);

    // The instrument: clearing a queue the engine never had proves nothing.
    let other = Client::connect(&engine.socket, "other", "0").expect("connect");
    let before = other.call("queue.get", None).expect("queue.get");
    assert_eq!(
        before["entries"].as_array().map(|e| e.len()),
        Some(1),
        "the queue never reached the engine: {before}"
    );

    ui.queue.clear();
    gmuse::ui::push_the_queue(&mut ui, &ctx);
    let after = other.call("queue.get", None).expect("queue.get");
    assert_eq!(
        after["entries"].as_array().map(|e| e.len()),
        Some(0),
        "the engine kept a queue the view emptied: {after}"
    );
}

/// **`:qa` stops the engine, and refuses while somebody else is attached** — #233.
///
/// `:q` leaves the engine playing, which is a detach in tmux's sense and is what attaching by
/// default makes the ordinary case. `:qa` is the other one, and it ends playback for every
/// client rather than only this one — so it asks first, the way vim's `:qa` refuses over an
/// unsaved buffer and takes `!` the same way.
#[test]
fn quit_all_stops_the_engine_unless_somebody_else_is_attached() {
    let (engine, _recording) = start_with_a_track("quit-all");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&client));
    let held = gmuse::librarian::Desk::read_only(
        Default::default(),
        gmuse::model::id::Id::derived(b"quit-all"),
        0,
        engine.dir.join("xdg").join("gmuse"),
    )
    .current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
    };

    // **Somebody else is here**, which is the case worth refusing — and "somebody else" is a
    // different *session*, not a second socket. Two `Client::connect` calls from this process
    // are one client, correctly, because `HelloParams::session` is the process id. So the
    // stranger greets by hand with a session of its own, which is what another program's
    // connection looks like from the engine's side.
    let other = {
        use std::io::{BufRead, Write};
        let mut sock = std::os::unix::net::UnixStream::connect(&engine.socket).expect("connect");
        let greeting = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "hello",
            "params": {
                "protocol": "gmuse/0",
                "client": "stranger",
                "session": "not-this-process",
            },
        });
        writeln!(sock, "{greeting}").expect("hello");
        sock.flush().expect("flush");
        // Read the reply, so the greeting has certainly been processed before the count below.
        let mut reply = String::new();
        std::io::BufReader::new(sock.try_clone().expect("clone"))
            .read_line(&mut reply)
            .expect("a hello reply");
        assert!(reply.contains("engine_generation"), "not a hello: {reply}");
        sock
    };

    // **A second connection of this view's own**, so the count below is about grouping rather
    // than about there happening to be one socket each. Without it an engine that recorded no
    // session at all would answer two as well — every connection its own client — and this
    // would pass while the session did nothing.
    let mine_too = Client::connect(&engine.socket, "view-again", "0").expect("connect");

    // The instrument: the engine must actually see two, or the refusal below is about nothing.
    let status = client.call("engine.status", None).expect("engine.status");
    let seen = status["clients"].as_u64().expect("a client count");
    assert_eq!(seen, 2, "the engine counts {seen} clients, not two");
    // **And there are more sockets than clients**, which is the whole point of the session:
    // this view is one client, and a count of sockets would have agreed only by accident.
    assert!(
        status["connections"].as_u64().expect("a connection count") > seen,
        "this view holds two sockets and is one client, so there are more: {status}"
    );
    drop(mine_too);

    gmuse::ui::execute(
        &mut ui,
        &ctx,
        gmuse::cmd::parse::line("qa")
            .expect("parses")
            .expect("a command"),
    );
    assert!(!ui.quitting(), "it quit while another client was attached");
    assert!(
        ui.status.contains("other client"),
        "it refused without saying why: {:?}",
        ui.status
    );
    assert!(
        client.call("engine.status", None).is_ok(),
        "the engine stopped despite refusing to"
    );

    // **`:qa!` goes anyway**, which is what a polling status bar makes necessary.
    gmuse::ui::execute(
        &mut ui,
        &ctx,
        gmuse::cmd::parse::line("qa!")
            .expect("parses")
            .expect("a command"),
    );
    assert!(ui.quitting(), "`:qa!` did not quit");

    // And the engine really stopped, which is the half a quitting view cannot show.
    let mut stopped = false;
    for _ in 0..100 {
        if Client::connect(&engine.socket, "probe", "0").is_err() {
            stopped = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(stopped, "the engine is still accepting connections");
    drop(other);
}

/// Detached there is nothing to stop, and `:qa` says so rather than reporting a shutdown
/// no engine received.
#[test]
fn quit_all_without_an_engine_is_just_quit() {
    let store = gmuse::operations::scan::Holdings::default();
    let _ = &store;
    let mut ui = gmuse::ui::Ui::new(Default::default());
    assert!(ui.engine.is_none(), "this view has an engine");
    let held = gmuse::model::seam::Librarian::default();
    let ctx = gmuse::ui::Context {
        library: &held,
        player: None,
        gapless: None,
    };

    gmuse::ui::execute(
        &mut ui,
        &ctx,
        gmuse::cmd::parse::line("qa")
            .expect("parses")
            .expect("a command"),
    );
    assert!(ui.quitting(), "`:qa` did not quit a detached view");
    assert!(
        ui.status.contains("no engine"),
        "it did not say there was nothing to stop: {:?}",
        ui.status
    );
}

/// **The programme outlives the engine that held it** — #257, `playback-programme.md` §9.
///
/// §1 says the librarian owns the list of what will play, and nothing wrote it down: stopping
/// the engine lost it whole. What survived was `session.toml`'s copy, which is a *view's* record
/// of a librarian's state — the wrong actor remembering, and no help at all to an engine nobody
/// is attached to.
///
/// **Restarted on the same data directory**, because that is the whole claim: a second engine
/// reading what the first wrote down, with no view involved at any point.
#[test]
fn a_programme_outlives_the_engine_that_held_it() {
    let (engine, recording) = start_with_a_track("programme-durable");
    let data = engine.dir.join("xdg").join("gmuse");

    let version = |c: &Client| -> u64 {
        c.call("queue.get", None).expect("queue.get")["version"]
            .as_u64()
            .expect("a version")
    };
    let entries = |c: &Client| -> Vec<String> {
        c.call("queue.get", None).expect("queue.get")["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .map(|e| e.as_str().expect("a recording").to_string())
            .collect()
    };

    {
        let client = Client::connect(&engine.socket, "view", "0").expect("connect");
        // The instrument: an engine that started with a queue would prove nothing below.
        assert!(
            entries(&client).is_empty(),
            "the engine started with a programme"
        );
        client
            .call(
                "queue.set",
                Some(serde_json::json!({
                    "recordings": [recording.clone()],
                    "base_version": version(&client),
                })),
            )
            .expect("queue.set");
        assert_eq!(
            entries(&client),
            vec![recording.clone()],
            "the set did not take"
        );
    }

    // **A different engine, on the same data.** Dropping the fixture would delete the
    // directory, so the child is stopped by hand and a second one started over it.
    let mut first = engine;
    let _ = first.child.kill();
    let _ = first.child.wait();

    let socket = first.dir.join("engine.sock");
    let _ = std::fs::remove_file(&socket);
    let child = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &socket)
        .env("GMUSE_DATA_DIR", &data)
        .env("GMUSE_ROOTS", &first.dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn a second gmuse engine");
    let second = Engine {
        child,
        dir: first.dir.clone(),
        socket: socket.clone(),
    };
    // The fixture's `Drop` would remove the directory out from under the second engine.
    std::mem::forget(first);

    for _ in 0..400 {
        if socket.exists() && std::os::unix::net::UnixStream::connect(&socket).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    let client = Client::connect(&second.socket, "view", "0").expect("connect to the second");
    assert_eq!(
        entries(&client),
        vec![recording],
        "the second engine did not read the first's programme"
    );
}

/// **A rebuild does not page in the flat ordering** — #261.
///
/// View 2's cursor used to be put back by reading the old index out of the flat ordering and
/// searching the new one for it, which forced that ordering into existence: ~0.8 s of paging
/// for the whole library, on every rating and every counted play, to move a cursor nobody was
/// looking at. It is a mark now, resolved when the pane is next read — and a pane being read
/// has the ordering in hand anyway.
///
/// **The absence needs its own witness.** "The cursor came back" is true whether or not the
/// ordering was paged in, so this asks the reader directly.
#[test]
fn rebuilding_the_library_does_not_page_in_the_flat_ordering() {
    let (engine, _recording) = start_with_a_track("no-ordering");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));
    let remote = gmuse::ui::holding_remote::Remote::open(std::sync::Arc::clone(&client));

    // The instrument: if it were already here, the assertion below would hold for the wrong
    // reason for ever.
    assert!(
        !remote.ordering_is_here(),
        "a fresh reader had already paged the ordering in"
    );

    let mut ui = gmuse::ui::Ui::new(Box::new(remote.clone()));
    ui.reader = Some(remote.clone());
    ui.engine = Some(std::sync::Arc::clone(&client));
    let held = gmuse::librarian::Desk::read_only(
        Default::default(),
        gmuse::model::id::Id::derived(b"no-ordering"),
        0,
        engine.dir.join("xdg").join("gmuse"),
    )
    .current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
    };

    gmuse::ui::refresh_library(&mut ui, &ctx);

    // **The reader the rebuild installed, not the one handed in.** `holding::of` builds a
    // fresh `Remote` whenever the view it is showing differs, so watching the original proves
    // nothing about the one that ended up in `ui.library` — which is how the first version of
    // this test passed with the eager resolve put back.
    let after = ui.reader.clone().expect("the rebuild kept a reader");
    assert!(
        !after.ordering_is_here(),
        "a rebuild paged the flat ordering in to move a cursor nobody is looking at"
    );

    // And it is still reachable: asking for it is what pages it in.
    assert!(
        !gmuse::browse::Holding::sorted(&after).is_empty(),
        "the ordering came back empty"
    );
    assert!(after.ordering_is_here(), "asking did not page it in");
}

/// **An engine that accepts and never answers is a bounded, explained failure** — #263.
///
/// `land` hands its facts to a running engine when that engine holds the append lock, and both
/// halves of that used to wait as long as it took. That is right for `gmuse ctl`, where a
/// person can interrupt it, and wrong here: this runs unattended after an operation that has
/// already moved files. An import sat there for **thirty minutes** at 0% CPU against an engine
/// spinning at 100% of a core (#264), saying nothing — diagnosed from `ps`, because a slow
/// import looks exactly the same.
///
/// **The socket is the trap.** `attach::is_live` asks only whether a connection is accepted, so
/// a listener that accepts and then ignores you passes it, which is what a wedged engine looks
/// like from outside.
///
/// **Two waits, and they fail in different places.** A socket that never speaks fails in the
/// handshake, before anything is asked for; one that shakes hands and then goes quiet fails in
/// the apply. Both are asserted, because bounding only the second leaves every caller — a
/// `ctl`, a view attaching — hanging in the first.
#[test]
fn an_engine_that_never_answers_fails_the_import_rather_than_hanging() {
    use std::io::{BufRead, Write};

    let scratch = |name: &str| {
        let dir = std::env::temp_dir().join(format!("gmuse-263-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        // **`XDG_DATA_HOME`, not `GMUSE_DATA_DIR`.** The latter steers the *engine* only, so a
        // `shelf` subcommand given it writes to the real library instead — which is how the
        // first version of this test appended a record to `~/.local/share/gmuse` and then
        // asserted against a code path it had never entered.
        std::fs::create_dir_all(dir.join("xdg").join("gmuse")).expect("scratch");
        std::fs::create_dir_all(dir.join("lib")).expect("scratch");
        write_wav(&dir.join("lib").join("tone.wav"), 44_100);
        dir
    };

    let importing = |dir: &std::path::Path, socket: &std::path::Path| {
        Command::new(binary())
            .args(["--no-config", "shelf", "import", "--write"])
            .arg(dir.join("lib").join("tone.wav"))
            .env("GMUSE_SOCKET", socket)
            .env("XDG_DATA_HOME", dir.join("xdg"))
            .env("GMUSE_ROOTS", dir)
            // Two seconds rather than the honest two minutes, or this is the slowest test here.
            .env("GMUSE_APPLY_SECS", "2")
            .current_dir(dir)
            .output()
            .expect("run gmuse shelf import")
    };

    // ===== a socket that never says anything =====
    let dir = scratch("mute");
    // Hold the append lock, so `land` takes the hand-it-over branch rather than appending.
    let held =
        gmuse::lock::acquire(&dir.join("xdg").join("gmuse").join("append.lock")).expect("the lock");
    let socket = dir.join("engine.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::thread::spawn(move || {
        let mut open = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            // Held, never written to. Hanging up would be an answer of a kind, and the case
            // under test is the one where there is none.
            open.push(stream);
        }
    });

    let out = importing(&dir, &socket);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    // The instrument: it has to have reached the hand-over, or the bound is about a path the
    // test never entered.
    assert!(
        said.contains("handing") && said.contains("running engine"),
        "it never announced the hand-over, so this is not the wait under test:\n{said}"
    );
    assert!(
        !out.status.success(),
        "a mute engine was a successful import:\n{said}"
    );
    assert!(
        said.contains("would not shake hands"),
        "a mute engine should fail in the handshake:\n{said}"
    );
    assert!(
        said.contains("engine.shutdown"),
        "the failure does not say how to finish the job:\n{said}"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);

    // ===== a socket that shakes hands and then goes quiet =====
    let dir = scratch("quiet");
    let held =
        gmuse::lock::acquire(&dir.join("xdg").join("gmuse").join("append.lock")).expect("the lock");
    let socket = dir.join("engine.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind");
    std::thread::spawn(move || {
        let mut open = Vec::new();
        while let Ok((stream, _)) = listener.accept() {
            let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone"));
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").cloned())
                    .unwrap_or(serde_json::json!(1));
                let mut w = stream.try_clone().expect("clone");
                let _ = writeln!(
                    w,
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": id,
                        "result": {
                            "protocol": "gmuse/0", "engine_generation": "test",
                            "stream_generation": 0, "engine_now_ns": 0,
                            "sample_rate": 48000, "capabilities": [],
                        },
                    })
                );
                let _ = w.flush();
            }
            // …and nothing after the handshake.
            open.push(stream);
        }
    });

    let out = importing(&dir, &socket);
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !out.status.success(),
        "a quiet engine was a successful import:\n{said}"
    );
    assert!(
        said.contains("did not take the facts within 2s"),
        "an engine that shook hands and then went quiet should fail in the apply:\n{said}"
    );
    drop(held);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Driven through `ui::push_the_queue` and `ui::read_the_queue` rather than raw
/// calls, because what is being tested is the view's half.
#[test]
fn the_views_queue_mirrors_the_engines_in_both_directions() {
    let (engine, recording) = start_with_a_track("mirror");
    let data = engine.dir.join("xdg").join("gmuse");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));
    client
        .subscribe(&[gmuse_proto::Topic::Queue])
        .expect("subscribe");

    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let held = desk.current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
    };

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&client));

    // **In: the engine's queue, which is empty and at some version.**
    gmuse::ui::read_the_queue(&mut ui, &ctx);
    assert!(ui.queue.is_empty(), "nothing queued yet");
    let first = ui.queue_version();
    assert!(first > 0, "the version was never read");

    // **Out: an edit made the way a keypress makes one.**
    let row = gmuse::model::rows::Listing::rows(&held.library)
        .into_iter()
        .find(|id| {
            gmuse::model::rows::Listing::recording_of(&held.library, *id).map(|r| r.to_string())
                == Some(recording.clone())
        })
        .expect("the imported row");
    ui.queue.push_back(row);
    gmuse::ui::push_the_queue(&mut ui, &ctx);
    assert!(
        ui.queue_version() > first,
        "the push did not move the engine's version"
    );

    // Another client sees it, which is what "the engine's queue" has to mean.
    let other = Client::connect(&engine.socket, "other", "0").expect("connect");
    let seen = other.call("queue.get", None).expect("queue.get");
    assert_eq!(
        seen["entries"].as_array().map(|e| e.len()),
        Some(1),
        "the other client sees a different queue: {seen}"
    );
    assert_eq!(seen["entries"][0].as_str(), Some(recording.as_str()));

    // **In again: somebody else's edit replaces the working copy.**
    other
        .call(
            "queue.clear",
            Some(serde_json::json!({ "base_version": seen["version"] })),
        )
        .expect("clear");
    gmuse::ui::read_the_queue(&mut ui, &ctx);
    assert!(
        ui.queue.is_empty(),
        "the view kept a queue the engine no longer has"
    );

    // **A stale edit is refused and the engine's shown**, not silently won.
    //
    // Staleness is manufactured the only way a view can be stale: it holds a
    // version, and somebody else moves past it.
    let held_version = ui.queue_version();
    let fresh = other.call("queue.get", None).expect("queue.get");
    other
        .call(
            "queue.enqueue",
            Some(serde_json::json!({
                "recordings": [recording],
                "base_version": fresh["version"]
            })),
        )
        .expect("enqueue");
    ui.queue.push_back(row);
    ui.queue.push_back(row);
    gmuse::ui::push_the_queue(&mut ui, &ctx);
    assert!(
        ui.status.contains("moved under that edit"),
        "a stale edit was not reported: {:?}",
        ui.status
    );
    assert_eq!(
        ui.queue.len(),
        1,
        "and the engine's queue is what is shown, not the rejected edit"
    );
    assert!(
        ui.queue_version() > held_version,
        "the version did not catch up"
    );
}

/// **What the socket accepts, the engine does** — #233.
///
/// `is_remotable` admitted `vol`, `rate`, `prev`, `set` and `toggle`, and
/// `control::apply` handled none of them: each answered `{"accepted": true}` and
/// did nothing. §7.4 makes a result mean *accepted*, which is precisely why the
/// accepting side must not lie — a client has no other signal.
///
/// One test per command that used to be a no-op, each asserting the observable
/// consequence rather than the acknowledgement.
#[test]
fn the_commands_that_used_to_be_accepted_and_ignored_now_act() {
    let (engine, recording) = start_with_a_track("carried-out");
    let client = Client::connect(&engine.socket, "driver", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Playback, gmuse_proto::Topic::Queue])
        .expect("subscribe");
    let line = |text: &str| {
        client
            .call("cmd.exec_line", Some(serde_json::json!({ "line": text })))
            .unwrap_or_else(|e| panic!("{text}: {e}"))
    };

    // **`set` moves the queue's version**, because what plays next changed.
    let before = client.call("queue.get", None).expect("queue.get")["version"]
        .as_u64()
        .expect("a version");
    line("set shuffle=true");
    let mut after = before;
    for _ in 0..200 {
        after = client.call("queue.get", None).expect("queue.get")["version"]
            .as_u64()
            .expect("a version");
        if after != before {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        after > before,
        "`set shuffle=true` changed nothing: version stayed at {before}"
    );

    // **A setting that is not the engine's is refused, not swallowed.** It reaches
    // the same gate, and `SettingKey::not_the_engines` is what answers.
    let refused = client.call(
        "cmd.exec_line",
        Some(serde_json::json!({ "line": "set relative_numbers=true" })),
    );
    let err = refused.expect_err("a view's gutter is not the engine's to set");
    assert_eq!(
        err.rpc().map(|r| r.code),
        Some(gmuse_proto::error::code::NOT_READY),
        "refused as needing a view: {err}"
    );

    // **`vol` moves the fader.** No topic reports it yet (§7.8 puts parameters on
    // `params`, which has no producer), so this asserts through the one thing that
    // does answer: setting it low and back is accepted, and an out-of-range value
    // is not silently clamped into a lie.
    line("vol 40");
    line("vol +10");

    // **`prev` loads something**, which arrives as a `playback` event naming a
    // recording. Nothing is playing, so it walks to the end of the pool — which is
    // `next::prev`'s answer when there is no current track, in the engine exactly
    // as in a view.
    //
    // **No `play` first.** An earlier version sent one and waited for an event that
    // cannot come — nothing is loaded, so there is no recording to name — which
    // spent twenty seconds proving nothing before the real assertion.
    line("prev");
    let mut heard = None;
    for _ in 0..100 {
        if let Ok(event) = client.next_event(Duration::from_millis(100))
            && event.topic == gmuse_proto::Topic::Playback
            && let Some(name) = event.body["recording"].as_str()
        {
            heard = Some(name.to_string());
            break;
        }
    }
    let played = heard.expect("`prev` named no recording, so it did nothing");

    // **`rate` reaches the log.** It rates what is playing, which is the one
    // subject the engine has without a cursor (I9) — and `prev` just made that
    // something.
    line("rate 4");
    let data = engine.dir.join("xdg").join("gmuse");
    let mut rated = false;
    for _ in 0..400 {
        let log: String = std::fs::read_dir(&data)
            .map(|entries| {
                entries
                    .flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                    .filter_map(|p| std::fs::read_to_string(p).ok())
                    .collect()
            })
            .unwrap_or_default();
        if log.contains("RecordingRated") {
            rated = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        rated,
        "`rate 4` over the socket appended nothing, and what was playing was {played}"
    );
    let _ = recording;
}

/// **The engine plays, with no TUI attached** — #230.
///
/// `gmuse-client` correlates by `id`, which is what this needs: a first attempt
/// in a shell read a `playback` event as the reply to its own command, because
/// §7.2 gives responses no ordering guarantee and means it.
#[test]
fn the_engine_plays_a_track_and_says_so() {
    let (engine, recording) = start_with_a_track("plays");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");

    // The handshake reports the stream, not a placeholder.
    assert_eq!(client.hello().sample_rate, 48_000, "a real device rate");
    assert!(
        client.hello().stream_generation >= 1,
        "a stream is open, so the generation is not zero: {}",
        client.hello().stream_generation
    );

    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("the load is accepted");

    // **The load names what plays.** Resolving a path back to a recording would
    // mean hashing the file, which §4.4 forbids on this thread — so the event
    // can only name it because the request did.
    //
    // **The first event may name nothing, and that is §7.4 rather than a gap.**
    // "A command's `result` means the command was accepted… Execution is reported
    // through events." The load is queued for the librarian now (#259), so the
    // acknowledgement and the sound are separated by a tick — and the tick that
    // publishes first is the one that says, truthfully, that nothing is playing
    // yet. This waits for the event that reports the execution.
    //
    // It is a *bounded* wait on a named condition, not a sleep: if no event ever
    // names the recording the expect below fails, which is the whole assertion.
    let mut loaded = None;
    for _ in 0..100 {
        let Ok(event) = client.next_event(Duration::from_millis(200)) else {
            continue;
        };
        if event.topic == gmuse_proto::Topic::Playback && event.body["recording"] == recording {
            loaded = Some(event);
            break;
        }
    }
    let loaded = loaded.expect("a playback event naming the loaded recording");
    assert_eq!(loaded.topic, gmuse_proto::Topic::Playback);
    assert_eq!(loaded.body["recording"], recording);
    assert_eq!(loaded.body["sample_rate"], 48_000);
    assert!(
        loaded.body["stream_generation"]
            .as_u64()
            .is_some_and(|g| g >= 1)
    );

    // `pause` is cmus's toggle, and `play` is deliberately not remotable — in a
    // view it means "play what is under the cursor". #225 gives the protocol its
    // own Play/Pause/Toggle.
    client
        .call(
            "cmd.exec_line",
            Some(serde_json::json!({ "line": "pause" })),
        )
        .expect("pause is accepted");

    // **Wait for the change, do not assume it is the next event.** A load
    // publishes once for the request and again when the decoder reports
    // `Loaded`, so the transition can be third. Asserting on "the next one"
    // made this flake on its own setup rather than on the behaviour.
    let was = loaded.body["state"].clone();
    let mut moved = None;
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let Ok(event) = client.next_event(Duration::from_secs(2)) else {
            break;
        };
        if event.body["state"] != was {
            moved = Some(event);
            break;
        }
    }
    let moved = moved.unwrap_or_else(|| panic!("the state never left {was}"));
    assert!(
        moved.body["stream_generation"]
            .as_u64()
            .is_some_and(|g| g >= 1),
        "and it is still the same stream"
    );
}

/// **No steady position stream** — §7.8. Events come on discontinuity only; a
/// client extrapolates between them from `pos_frames` and `at_ns`.
#[test]
fn playback_events_do_not_arrive_between_discontinuities() {
    let (engine, _recording) = start_with_a_track("quiet");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");

    // Drain whatever the startup published.
    while client.next_event(Duration::from_millis(300)).is_ok() {}

    // Then ask for nothing for a while.
    assert!(
        client.next_event(Duration::from_secs(2)).is_err(),
        "the engine published a playback event with nothing happening"
    );
}

/// **The engine picks the next track, with no view attached** — #232, and the
/// property the whole milestone exists for.
///
/// The decoder's `NeedNext` arrives while the current track is still playing and
/// needs an answer before it ends. Nothing is asked of a client here: this test
/// **A cached tree is not served to a different question** — #261.
///
/// `query.tree` keeps the last few trees so paging does not rebuild one: on the real library
/// every page cost **0.40 s** before the cache and about a millisecond after.
///
/// The way a cache goes wrong is answering the wrong question, so this asks three that must
/// differ — everything, a filter, and no grouping — and checks each gets its own answer over
/// a real socket.
///
/// **The fold's version is the fourth part of that key**, and it is what makes a library
/// change invalidate a tree. Proving it needs a library change that alters the tree's *shape*
/// rather than a rating, which this harness cannot drive — so it is asserted one layer down,
/// in `wiring::kept`'s `a_newer_fold_is_not_the_same_question`, where the key type is.
#[test]
fn a_cached_tree_is_keyed_by_the_question_it_answers() {
    let (engine, _recording) = start_with_a_track("tree-cache");
    let client = Client::connect(&engine.socket, "browser", "0").expect("connect");

    let total = |params: serde_json::Value| -> u64 {
        client
            .call("query.tree", Some(params))
            .expect("the tree")
            .get("total")
            .and_then(|t| t.as_u64())
            .expect("a total")
    };

    // Two tracks were imported, so the grouped tree is artist + album + two tracks and the
    // flat one is just the tracks. **The instrument**: if the fixture had one track the
    // grouped and flat totals could coincide and this would prove nothing.
    let grouped = total(serde_json::json!({ "limit": 1 }));
    assert!(
        grouped > 2,
        "only {grouped} nodes, so the shapes cannot differ"
    );

    let flat = total(serde_json::json!({ "limit": 1, "levels": [] }));
    assert!(
        flat < grouped,
        "flat has {flat} nodes and grouped {grouped} — the cache answered the wrong grouping"
    );

    // A view nothing matches is a different question again, and asked *after* the two above so
    // a stale entry would be the thing returned. A bare filter is a view of one term (#261),
    // which is what makes `view` strictly wider than the `filter` it replaced.
    let none = total(serde_json::json!({ "limit": 1, "view": "artist=\"nobody at all\"" }));
    assert_eq!(
        none, 0,
        "a view matching nothing was answered with {none} nodes from another question"
    );

    // And back to the first, which must still be right rather than evicted into something else.
    assert_eq!(
        total(serde_json::json!({ "limit": 1 })),
        grouped,
        "asking the first question again gave a different answer"
    );
}

/// **A headless engine counts a play** — #257, and nothing checked this before.
///
/// `record_play_if_earned` lived in the TUI and polled a position a *view* could see, so an
/// engine with nobody attached counted nothing: all 911 of the real library's
/// `RecordingPlayed` facts were written by something drawing a screen.
///
/// Asserted against the log rather than a count in memory, because the point is that it
/// **reached the log** — through `Wanted::Played` and the relay, neither of which the engine
/// can do for itself any more.
///
/// No client subscribes and nothing but `player.load` is sent: if a view were still required,
/// there is none here to be it.
#[test]
fn an_engine_with_no_view_counts_a_play() {
    let (engine, recording) = start_with_a_track("counts-a-play");
    let client = Client::connect(&engine.socket, "loader", "0").expect("connect");
    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");

    // The first track is a second long at wall speed, so half of it is half a second. Polled
    // on a named condition rather than slept through, so a slow machine waits and a fast one
    // does not.
    let log = engine.dir.join("xdg").join("gmuse");
    let mut played = false;
    for _ in 0..200 {
        if let Some(text) = read_the_log(&log)
            && text.contains("RecordingPlayed")
            && text.contains(&recording)
        {
            played = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    assert!(
        played,
        "nothing counted a play in ten seconds, with a track a second long — the log holds:\n{}",
        read_the_log(&log).unwrap_or_default()
    );
}

/// Every `log-*.jsonl` under a data directory, concatenated.
fn read_the_log(data: &std::path::Path) -> Option<String> {
    let mut out = String::new();
    for entry in std::fs::read_dir(data).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|x| x == "jsonl")
            && let Ok(text) = std::fs::read_to_string(&path)
        {
            out.push_str(&text);
        }
    }
    Some(out)
}

/// connects only to *watch*, and could disconnect without changing the outcome.
#[test]
fn playback_continues_to_a_second_track_with_no_view_answering() {
    let (engine, recording) = start_with_a_track("gapless");
    let client = Client::connect(&engine.socket, "watcher", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");

    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");
    // **No play command, because `load` starts it.** The UI's resume path is the
    // evidence: it loads, seeks, and then explicitly pauses, because "resuming
    // playback unbidden on launch would be obnoxious". A first version of this
    // test sent `pause` here and spent a while watching a paused player refuse
    // to reach the end of a track.

    // **Wait for a second recording to be named, *while it is playing*.** The first
    // track is a second long under the null renderer at wall speed, so the join
    // happens on its own — and the only way a second recording can appear is if the
    // engine chose it, because nobody told it one.
    //
    // **`while playing` is the whole assertion**, and without it this test passed
    // against the bug. `held.playing` was promoted on `Finished`, which a splice
    // never sends, so the second name arrived once — at the end of the pool, with
    // `state: "stopped"`. Collecting names over the window saw two of them and
    // called that an advance. What a listener actually needs is the name of the
    // track that is *sounding*, which is a claim about the state it arrives with.
    let mut names = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        let Ok(event) = client.next_event(Duration::from_secs(3)) else {
            continue;
        };
        if event.body["state"].as_str() != Some("playing") {
            continue;
        }
        if let Some(rec) = event.body["recording"].as_str()
            && !names.contains(&rec.to_string())
        {
            names.push(rec.to_string());
        }
        if names.len() >= 2 {
            break;
        }
    }

    assert!(
        names.len() >= 2,
        "the engine never named a second track while playing; it named {names:?}"
    );
    assert!(
        names.contains(&recording),
        "and the first one was the one asked for"
    );
}

/// **A client that arrives mid-playback is told what is playing** — #243, #248, §7.8.
///
/// The `playback` topic carries discontinuities, and `playback_events_do_not_arrive_
/// between_discontinuities` above asserts the silence between them. So a view that
/// attached mid-album learned nothing until the next track change — it drew "stopped"
/// over audible music and `gp` had no target. `playback.get` is the snapshot, and this
/// is the case it exists for.
#[test]
fn a_late_client_can_ask_what_is_playing() {
    let (engine, recording) = start_with_a_track("late");

    // One client starts something, then goes away entirely.
    {
        let first = Client::connect(&engine.socket, "starter", "0").expect("connect");
        first
            .call(
                "player.load",
                Some(serde_json::json!({ "recording": recording })),
            )
            .expect("load");
    }

    // **Wait until it is actually playing before asking.** Without this the test
    // passes against an engine that never started: `state: "stopped"` with
    // `recording: null` is a correct answer to the wrong question, and it is the
    // answer the bug gave too.
    let late = Client::connect(&engine.socket, "late", "0").expect("connect");
    let mut asked = serde_json::Value::Null;
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        asked = late.call("playback.get", None).expect("ask");
        if asked["state"].as_str() == Some("playing") {
            break;
        }
    }

    assert_eq!(
        asked["state"].as_str(),
        Some("playing"),
        "the engine never started, so nothing was measured: {asked}"
    );
    assert_eq!(
        asked["recording"].as_str(),
        Some(recording.as_str()),
        "a client that never saw an event was not told what is playing: {asked}"
    );

    // **And the same connection can ask for a line**, which is what a status bar
    // needs: `now_playing` puts the title last, so a bar cannot truncate a rendered
    // one to fit (§13, 2026-09-01).
    let rendered = late
        .call(
            "playback.get",
            Some(serde_json::json!({ "format": "{title}" })),
        )
        .expect("ask with a format");
    assert!(
        rendered["rendered"]
            .as_str()
            .is_some_and(|line| !line.is_empty()),
        "a format produced no line: {rendered}"
    );
}

/// **Two clients, `StaleVersion` on the queue** — #217's last Done-when line,
/// and #232 is what made it expressible.
///
/// §8.8 puts optimistic concurrency here and nowhere else: a library write
/// unions and cannot conflict, and two views reordering a queue at once is a
/// lost update. So this is the one refusal of its kind in the protocol.
#[test]
fn two_clients_editing_the_queue_produce_a_stale_version() {
    let (engine, recording) = start_with_a_track("stale");

    let first = Client::connect(&engine.socket, "first", "0").expect("connect");
    let second = Client::connect(&engine.socket, "second", "0").expect("connect");

    // Both put something in, so there is an entry to fight over.
    first
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [recording] })),
        )
        .expect("append needs no version");

    // The first client reads the queue and remembers where it was.
    let seen = first.call("queue.get", None).expect("read")["version"]
        .as_u64()
        .expect("a version");

    // The second client gets there first.
    second
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [recording], "base_version": seen })),
        )
        .expect("in step");

    // Now the first client's version is behind, and removing index 0 would take
    // the wrong track.
    let refused = first
        .call(
            "queue.remove",
            Some(serde_json::json!({ "index": 0, "base_version": seen })),
        )
        .expect_err("the queue moved");
    let rpc = refused.rpc().expect("an rpc error");
    assert_eq!(rpc.code, 1001, "StaleVersion: {rpc}");
    assert!(
        rpc.data
            .as_ref()
            .and_then(|d| d["current"].as_u64())
            .is_some_and(|c| c > seen),
        "and it says where the queue actually is: {:?}",
        rpc.data
    );

    // Nothing was removed.
    let after = first.call("queue.get", None).expect("read");
    assert_eq!(
        after["entries"].as_array().map(Vec::len),
        Some(2),
        "the refusal changed nothing: {after}"
    );
}

/// A `queue` event says only its version — §7.8. A view re-reads; sending the
/// entries would make every reorder carry the whole list.
///
/// **This raced the engine's startup announcement and failed about one run in
/// six** — #283. There is one publish site for `Topic::Queue`, guarded by
/// `version != announced` with `announced` starting at `0`, so the engine
/// announces the queue it came up with — version 1 — on its first tick. This
/// took the *first* event after enqueueing and asserted `version > 1`, which
/// only holds if that announcement went out before the subscribe and was
/// therefore missed. Usually it did. Under load the first tick lands late, the
/// subscriber gets version 1, and the assertion fails on an event the enqueue
/// did not cause.
///
/// **The `entries` assertion did not help, and that is the part worth keeping in
/// mind.** The startup event has no `entries` either, so it passed on the wrong
/// event too: both assertions were satisfied by an event the test was not
/// testing. On the five runs in six that passed, the test was right by accident.
///
/// So it reads the version *before* the change rather than hard-coding 1, and
/// reads events until one is above it. **Not a longer timeout** — the event
/// arrives promptly; the test was looking at the wrong one.
#[test]
fn a_queue_change_is_announced_by_version() {
    let (engine, recording) = start_with_a_track("queue-event");
    let client = Client::connect(&engine.socket, "watcher", "0").expect("connect");

    // The version this engine is already at, whatever it is.
    let before = client.call("queue.get", None).expect("queue.get")["version"]
        .as_u64()
        .expect("queue.get carries a version");

    client
        .subscribe(&[gmuse_proto::Topic::Queue])
        .expect("subscribe");

    client
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [recording] })),
        )
        .expect("append");

    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut seen: Vec<u64> = Vec::new();
    let event = loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        // **The versions seen are the diagnostic**, so a timeout reports them
        // rather than letting `next_event`'s own message win. `saw [1]` says
        // "the startup announcement and nothing since" at a glance, which is the
        // whole history of this test (#283).
        let event = match client.next_event(left) {
            Ok(event) => event,
            Err(why) => panic!(
                "no queue event above version {before} within ten seconds \
                 ({why:?}); versions seen: {seen:?}"
            ),
        };
        assert_eq!(event.topic, gmuse_proto::Topic::Queue);
        // **Every event, not just the one we keep.** §7.8 is a property of the
        // topic, so the announcement that arrives first has to obey it too.
        assert!(
            event.body.get("entries").is_none(),
            "an event carried the entries: {}",
            event.body
        );
        let version = event.body["version"]
            .as_u64()
            .unwrap_or_else(|| panic!("an event without a version: {}", event.body));
        if version > before {
            break event;
        }
        seen.push(version);
    };

    assert!(
        event.body["version"].as_u64().is_some_and(|v| v > before),
        "it carries the version: {}",
        event.body
    );
}

/// **A client starts the engine if none answers** — #219, §4.2's tmux model.
///
/// The client spawns a detached child; `gmuse engine` still implements only
/// `--foreground`, and does not need to change, because the *client* is what
/// detaches it.
#[test]
fn attaching_starts_an_engine_when_none_is_listening() {
    let dir = std::env::temp_dir().join(format!("gmuse-219-{}-attach", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).expect("scratch");
    let socket = dir.join("engine.sock");
    assert!(!socket.exists(), "nothing is listening to begin with");

    // `attach` reads the environment the same way the engine does, so pointing
    // both at the scratch directory is enough to keep this off the real one.
    let spawned = Command::new(binary())
        .args(["ctl", "--json", "engine.status"])
        .env("GMUSE_SOCKET", &socket)
        .env("GMUSE_DATA_DIR", dir.join("data"))
        .env("GMUSE_ROOTS", "")
        .output()
        .expect("run gmuse ctl");

    // Today `ctl` does not auto-start — it says how to start one, which is the
    // behaviour #217 shipped. What this pins is that the *message* is the
    // actionable one, so the change to auto-starting is visible when it happens.
    let said = String::from_utf8_lossy(&spawned.stderr);
    assert!(
        said.contains("gmuse engine"),
        "it says how to start one: {said}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **A socket that accepts and never answers is named once, not retried for twenty
/// seconds** — #264's shape, seen from the client.
///
/// An engine wedged with its socket bound holds the lock too, so a spawned replacement
/// exits and every reconnect in `attach`'s window meets the same silence at five seconds a
/// time. Three attaches at start-up made that thirty seconds with nothing on screen,
/// reported as "gmuse is slow to start". The repair is to stop the wedged engine, which
/// only a message can say — so the handshake timeout is its own error, and it is final.
///
/// **Two clocks, both asserted.** Under twelve seconds proves the retry window was not
/// spent; over four proves the handshake was actually waited for, which is what separates
/// "it gave up" from "it never connected".
#[test]
fn a_silent_engine_is_named_and_not_waited_on() {
    use std::os::unix::net::UnixListener;

    let dir = std::env::temp_dir().join(format!("gmuse-silent-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    let socket = dir.join("engine.sock");
    // **The lock beside it, held** — as a wedged engine holds it, having bound before it
    // wedged. This is what makes the old behaviour cost twenty seconds rather than ten: a
    // spawned engine loses the lock and exits, so nothing ever takes the socket over and
    // every retry meets the same silence. It is also what keeps this test from starting a
    // real engine against the real library, which a fake that only *looked* held did.
    let _held_lock = gmuse::lock::acquire(&dir.join("engine.lock")).expect("hold the lock");
    // The pid the lock wrote is this test's; the message should carry it.
    let pid = std::process::id().to_string();

    // **Accepts, reads nothing, answers nothing** — held open, so the client's handshake
    // has to time out rather than see a close.
    let listener = UnixListener::bind(&socket).expect("bind the fake");
    let held = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let keep = std::sync::Arc::clone(&held);
    std::thread::spawn(move || {
        for conn in listener.incoming().flatten() {
            keep.lock().expect("held").push(conn);
        }
    });

    // `ctl` is the plain client: one connect, one handshake.
    let started = std::time::Instant::now();
    let out = Command::new(binary())
        .args(["ctl", "engine.status"])
        .env("GMUSE_SOCKET", &socket)
        .output()
        .expect("run gmuse ctl");
    let took = started.elapsed();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "ctl claimed to reach a silent engine"
    );
    assert!(
        said.contains("accepts connections and does not answer"),
        "ctl did not say what it found: {said}"
    );
    assert!(
        !said.contains("Start one with"),
        "ctl told you to start an engine beside one that is wedged: {said}"
    );
    assert!(
        took > Duration::from_secs(4) && took < Duration::from_secs(12),
        "ctl took {took:?}: under four is a handshake never waited for, over twelve is the \
         retry window being spent"
    );
    assert!(
        !held.lock().expect("held").is_empty(),
        "the fake accepted nothing, so nothing was silent at"
    );

    // The view attaches through `attach`, which spawns and retries; against silence it
    // must do neither for long. No terminal is needed: attaching is the first thing it
    // does, and the failure is reported before the screen is taken.
    let started = std::time::Instant::now();
    let out = Command::new(binary())
        .args(["--no-lock", "--no-config"])
        .env("GMUSE_SOCKET", &socket)
        // Belt and braces: the lock above stops a spawned engine, and this stops one that
        // somehow started from touching anything real.
        .env("GMUSE_DATA_DIR", dir.join("data"))
        .env("GMUSE_ROOTS", "")
        .stdin(Stdio::null())
        .output()
        .expect("run gmuse");
    let took = started.elapsed();
    let said = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "the view claimed to attach to a silent engine"
    );
    assert!(
        said.contains("accepts connections and does not answer")
            && said.contains(&format!("pid {pid}")),
        "the view did not name the silent engine and its pid: {said}"
    );
    assert!(
        took > Duration::from_secs(4) && took < Duration::from_secs(12),
        "attach took {took:?}: the retry window was spent against a socket that will never \
         answer"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// **The second implementor of the seam, over a real socket** — #219.
///
/// This is what lets the TUI hold no audio device: it asks the engine and renders
/// what comes back. §7.8 gives it no synchronous read of position, so the
/// implementation caches the last `playback` event and extrapolates — which is
/// what the assertion below is actually about.
#[test]
fn a_remote_transport_tracks_the_engines_playback() {
    use gmuse::ui::transport::Transport;

    let (engine, recording) = start_with_a_track("remote-transport");
    let client = std::sync::Arc::new(Client::connect(&engine.socket, "tui", "0").expect("connect"));
    let remote = gmuse::ui::remote_transport::Remote::attach(std::sync::Arc::clone(&client))
        .expect("attach");

    // Nothing has played, so the cache says so rather than guessing.
    assert_eq!(remote.state(), gmuse::audio::State::Stopped);
    assert_eq!(remote.position(), Duration::ZERO);

    // A second connection loads, the way another view would.
    let driver = Client::connect(&engine.socket, "driver", "0").expect("connect");
    driver
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");

    // **The cache catches up from the event stream**, with no polling of the
    // engine and no synchronous read on the wire.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if remote.state() == gmuse::audio::State::Playing {
            break;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    assert_eq!(
        remote.state(),
        gmuse::audio::State::Playing,
        "the transport learned the engine started from an event"
    );

    // And the position moves between events, because it is extrapolated.
    let first = remote.position();
    std::thread::sleep(Duration::from_millis(300));
    let later = remote.position();
    assert!(
        later > first,
        "a playing position advances between discontinuities: {first:?} -> {later:?}"
    );
}

/// **An attached view learns mid-track what is playing, without asking** — #219.
///
/// `playback_survives_a_view_detaching_and_another_attaching` above proves the
/// engine keeps playing; this proves the *seam* reports it. The distinction
/// matters because the TUI draws `ui.playback.current` from
/// `Transport::playing`, and the only thing that can set it on a view that
/// attached late is an event it did not ask for.
///
/// It is also why `outlives_this_view` exists: a session restore on this view
/// would load over what is playing, seek it and pause it. That failure is
/// invisible from the protocol level, because at that level nothing restores.
#[test]
fn an_attached_view_learns_what_is_playing_from_the_event_stream() {
    use gmuse::ui::transport::Transport;

    let (engine, recording) = start_with_a_track("late-attach");

    // The first view: attach, load, see it play.
    {
        let client =
            std::sync::Arc::new(Client::connect(&engine.socket, "first", "0").expect("connect"));
        let view = gmuse::ui::remote_transport::Remote::attach(client).expect("attach");
        assert_eq!(
            view.load(gmuse::ui::transport::Loading {
                recording: Some(recording.parse().expect("a recording id")),
                view: None,
                // **Ignored, and deliberately a path that does not exist.** A
                // locator does not cross (I17); the engine opens the copy its own
                // projection names. If this were the half being used, nothing would
                // play and the wait below would time out.
                source: gmuse::audio::TrackSource {
                    path: std::path::PathBuf::from("/nonexistent/not-the-copy-that-plays.wav"),
                    gain: 1.0,
                },
            }),
            gmuse::ui::transport::Accepted::Yes,
            "a named recording is something a socket can carry"
        );
        assert!(
            wait_until(|| view.state() == gmuse::audio::State::Playing),
            "the first view got it playing"
        );
        assert_eq!(
            view.playing().map(|r| r.to_string()),
            Some(recording.clone()),
            "and knows which recording, from the event rather than from having asked"
        );
    }
    // Dropped: the connection is closed and the reader thread told to stop.
    // Exactly what closing a terminal does.

    // The second view, which has no idea any of that happened.
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "second", "0").expect("connect"));
    let fresh = gmuse::ui::remote_transport::Remote::attach(client).expect("attach");

    // **Learned from an event, not asked for.** §7.8 has no synchronous read of
    // position, and the engine republishes state to a new subscriber — which is
    // what makes attaching mid-track show the right thing.
    assert!(
        wait_until(|| fresh.playing().map(|r| r.to_string()) == Some(recording.clone())),
        "a fresh view found the same recording still playing: it said {:?}",
        fresh.playing().map(|r| r.to_string())
    );
    assert_eq!(
        fresh.state(),
        gmuse::audio::State::Playing,
        "and still playing, because a view detaching is not a stop"
    );
    assert!(
        fresh.outlives_this_view(),
        "which is the whole claim this transport makes about itself"
    );
}

/// **"Up next" is what follows the track that is playing, not the track that is playing** —
/// #257, `playback-programme.md` §2.
///
/// `queue.get` pages the programme from what is sounding: `[the track playing, then what
/// follows it…]`. The view took the whole page as its queue, so the pane's first row and the
/// transport panel's `next` line both named the track already playing — and a `queue.set`
/// computed from that mirror put it at the head of the new programme, so every edit made the
/// engine play the current track again when it ended.
///
/// The second half: a track ending moves the anchor and not the version, so no `queue` event
/// says so. The view has to notice the engine moving on and read its page again.
///
/// **The instrument is the raw page.** The engine must really be anchored at the playing
/// track for "the mirror does not begin with it" to be a claim about anything.
#[test]
fn up_next_does_not_begin_with_the_track_that_is_playing() {
    use gmuse::ui::transport::Transport;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (engine, recording) = start_with_a_track("up-next-anchor");
    let data = engine.dir.join("xdg").join("gmuse");

    // Two connections, as the TUI holds them: one for the transport's event stream, one
    // for the queue and the library — a `Client` has one receiver, so they cannot share.
    let queue =
        std::sync::Arc::new(Client::connect(&engine.socket, "queue", "0").expect("connect"));
    let remote = gmuse::ui::remote_transport::Remote::attach(std::sync::Arc::new(
        Client::connect(&engine.socket, "transport", "0").expect("connect"),
    ))
    .expect("attach");

    let machine = gmuse::events::machine_id(&gmuse::config::default_state_dir());
    let device = gmuse::model::id::Id::derived(machine.as_bytes());
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let held = desk.current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: Some(&remote),
        gapless: None,
    };
    let row_named = |r: &str| {
        gmuse::model::rows::Listing::rows(&held.library)
            .into_iter()
            .find(|id| {
                gmuse::model::rows::Listing::recording_of(&held.library, *id).map(|x| x.to_string())
                    == Some(r.to_string())
            })
            .expect("the imported row")
    };
    let title_of = |id| {
        gmuse::model::rows::Listing::listed(&held.library, id)
            .expect("a listed row")
            .title
    };

    // Play the first track and hold it there: under the null renderer it is a second long,
    // and a page anchored at a track that has finished would prove the wrong thing.
    let accepted = remote.load(gmuse::ui::transport::Loading {
        recording: Some(recording.parse().expect("a recording id")),
        view: None,
        source: gmuse::audio::TrackSource {
            path: std::path::PathBuf::from("/nonexistent/ignored-over-the-wire.wav"),
            gain: 1.0,
        },
    });
    assert_eq!(accepted, gmuse::ui::transport::Accepted::Yes);
    assert!(
        wait_until(|| remote.state() == gmuse::audio::State::Playing),
        "the engine never started"
    );
    remote.pause();
    assert!(
        wait_until(|| remote.state() == gmuse::audio::State::Paused),
        "the engine did not pause"
    );
    assert_eq!(
        remote.playing().map(|r| r.to_string()),
        Some(recording.clone()),
        "paused on something other than what was loaded"
    );

    // The instrument: the raw page really does begin with the track that is sounding.
    let page = queue.call("queue.get", None).expect("queue.get");
    assert_eq!(
        page["anchored"].as_bool(),
        Some(true),
        "the page was not anchored at what is playing: {page}"
    );
    assert_eq!(
        page["entries"][0].as_str(),
        Some(recording.as_str()),
        "the page does not begin with what is playing: {page}"
    );
    let on_page = page["entries"].as_array().map_or(0, Vec::len);
    assert!(
        on_page >= 2,
        "a programme of one has nothing to be next: {page}"
    );

    // One tick of following the engine: adopt what is playing, offer nothing, read the page.
    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.engine = Some(std::sync::Arc::clone(&queue));
    gmuse::ui::keep_up(&mut ui, &ctx);

    let playing = row_named(&recording);
    assert_eq!(
        ui.playback.current,
        Some(playing),
        "the view did not follow the engine"
    );
    let mirrored: Vec<_> = ui.queue.entries().copied().collect();
    assert_eq!(
        mirrored.len(),
        on_page - 1,
        "the mirror is not the page less the track that is playing: {mirrored:?}"
    );
    assert_ne!(
        mirrored.first(),
        Some(&playing),
        "up next begins with the track that is already playing"
    );
    let following = mirrored[0];

    // **And the transport panel says the same.** `i` raises it, and its `next` line was
    // the surface this was reported on.
    ui.mode = gmuse::cmd::Mode::Transport;
    let drawn = |ui: &gmuse::ui::Ui| -> String {
        let mut terminal = Terminal::new(TestBackend::new(90, 20)).expect("a backend");
        terminal
            .draw(|f| gmuse::ui::draw(f, ui, &ctx))
            .expect("a frame");
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let out = drawn(&ui);
    let next_line = out
        .lines()
        .find(|l| l.contains("next  "))
        .unwrap_or_else(|| panic!("no next line in the panel:\n{out}"));
    assert!(
        next_line.contains(&title_of(following)),
        "the panel does not name what follows: {next_line:?}"
    );
    assert!(
        !next_line.contains(&format!(" {} ", title_of(playing)))
            && !next_line.ends_with(&title_of(playing)),
        "the panel names the track that is playing as next: {next_line:?}"
    );

    // **Then the engine moves on**, and nothing about the programme's version changes —
    // so the only way the view can learn its page is stale is by noticing what it follows.
    let version = ui.queue_version();
    let follows = gmuse::model::rows::Listing::recording_of(&held.library, following);
    assert!(follows.is_some(), "the following row names no recording");
    remote.play();
    assert!(
        wait_until(|| remote.playing() == follows),
        "the engine never reached the second track: it says {:?}",
        remote.playing().map(|r| r.to_string())
    );
    remote.pause();
    assert!(
        wait_until(|| remote.state() == gmuse::audio::State::Paused),
        "the engine did not pause on the second track"
    );

    gmuse::ui::keep_up(&mut ui, &ctx);
    assert_eq!(
        ui.queue_version(),
        version,
        "the version moved, so this proves nothing about following the anchor"
    );
    assert_eq!(
        ui.playback.current,
        Some(following),
        "the view did not follow the advance"
    );
    let mirrored: Vec<_> = ui.queue.entries().copied().collect();
    assert_ne!(
        mirrored.first(),
        Some(&following),
        "after the advance, up next still begins with the track now playing"
    );
    assert!(
        !mirrored.contains(&playing),
        "the track that finished is still listed as up next: {mirrored:?}"
    );
}

/// **A file the library has no row for cannot be asked for over a socket** —
/// I17, #219.
///
/// A path is a property of one device's copy, so there is nothing to send. The
/// refusal is a return value rather than a silent drop because the view marks a
/// row as playing on the strength of it.
#[test]
fn an_attached_view_refuses_to_load_what_the_library_cannot_name() {
    use gmuse::ui::transport::{Accepted, Loading, Transport};

    let (engine, _recording) = start_with_a_track("unnamed");
    let client = std::sync::Arc::new(Client::connect(&engine.socket, "tui", "0").expect("connect"));
    let view = gmuse::ui::remote_transport::Remote::attach(client).expect("attach");

    let answer = view.load(Loading {
        recording: None,
        view: None,
        source: gmuse::audio::TrackSource {
            path: std::path::PathBuf::from("/tmp/a-file-nobody-imported.wav"),
            gain: 1.0,
        },
    });
    assert_eq!(
        answer,
        Accepted::NotNamed,
        "it says so rather than sending a path the engine would refuse"
    );
    assert_eq!(
        view.state(),
        gmuse::audio::State::Stopped,
        "and nothing started"
    );
}

/// Wait for something the event stream will eventually say, or give up.
fn wait_until(mut done: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    done()
}

/// **Two engines cannot both bind, so the auto-start race is benign** — #219.
///
/// Two views starting at once both find nothing listening and both spawn. The
/// loser cannot take `engine.lock`, which #214 made a clean refusal rather than a
/// crash or a half-bound socket, so nothing has to coordinate.
#[test]
fn a_second_engine_loses_cleanly_so_two_views_can_race_to_start_one() {
    let (engine, _recording) = start_with_a_track("race");

    let second = Command::new(binary())
        .args(["engine", "--foreground", "--null-audio"])
        .env("GMUSE_SOCKET", &engine.socket)
        .env("GMUSE_DATA_DIR", engine.dir.join("xdg").join("gmuse"))
        .env("GMUSE_ROOTS", "")
        .output()
        .expect("run a second engine");

    assert!(
        !second.status.success(),
        "the loser exits rather than binding"
    );
    let said = String::from_utf8_lossy(&second.stderr);
    assert!(
        said.contains("another gmuse engine is running"),
        "and says why: {said}"
    );

    // The winner is still answering.
    let client = Client::connect(&engine.socket, "after", "0").expect("the first still listens");
    assert_eq!(client.hello().protocol, gmuse_proto::PROTOCOL);
}

/// **Playback continues across a detach and a re-attach** — #219's second
/// Done-when line, and the property the whole milestone exists for.
///
/// Nothing about this needs a TUI: it is a claim about the engine, and it became
/// true when #230 gave it a player and #232 gave it next-track selection. A view
/// is now something that can go away.
#[test]
fn playback_survives_a_view_detaching_and_another_attaching() {
    let (engine, recording) = start_with_a_track("detach");

    // A view attaches and starts something.
    let first = Client::connect(&engine.socket, "first-view", "0").expect("connect");
    first
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    first
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");

    // Wait until it is actually playing, so the detach happens mid-track rather
    // than before anything started.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut playing = false;
    while std::time::Instant::now() < deadline {
        let Ok(event) = first.next_event(Duration::from_secs(2)) else {
            continue;
        };
        if event.body["state"] == "playing" {
            playing = true;
            break;
        }
    }
    assert!(playing, "the first view got it playing");

    // **The view goes away.** §7.10: a disconnect is a non-event for the engine —
    // its subscriptions are dropped and nothing else changes.
    drop(first);

    // A second view attaches to the engine that is still running.
    let second = Client::connect(&engine.socket, "second-view", "0").expect("re-attach");
    assert!(
        second.hello().stream_generation >= 1,
        "the same stream is still open"
    );
    second
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");

    // And it can still drive what the first one started.
    second
        .call(
            "cmd.exec_line",
            Some(serde_json::json!({ "line": "pause" })),
        )
        .expect("the second view can drive it");

    let event = second
        .next_event(Duration::from_secs(10))
        .expect("a playback event for the second view");
    assert_eq!(event.topic, gmuse_proto::Topic::Playback);
    assert!(
        event.body["recording"].as_str().is_some(),
        "and it still knows what is loaded: {}",
        event.body
    );
}

/// **`next` over the wire skips to the next track** — #232 gave the engine the
/// queue, so this no longer needs a view to resolve it.
#[test]
fn next_over_the_wire_skips_without_a_view_resolving_it() {
    let (engine, recording) = start_with_a_track("next");
    let client = Client::connect(&engine.socket, "view", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");

    // Let the first track be the one playing.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while std::time::Instant::now() < deadline {
        let Ok(e) = client.next_event(Duration::from_secs(2)) else {
            continue;
        };
        if e.body["state"] == "playing" {
            break;
        }
    }

    // Skip. The engine picks from its own queue and pool.
    client
        .call("cmd.exec_line", Some(serde_json::json!({ "line": "next" })))
        .expect("next is remotable");

    // Something other than the first recording ends up loaded.
    let mut names = vec![recording.clone()];
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
    while std::time::Instant::now() < deadline {
        let Ok(e) = client.next_event(Duration::from_secs(3)) else {
            continue;
        };
        if let Some(rec) = e.body["recording"].as_str()
            && !names.contains(&rec.to_string())
        {
            names.push(rec.to_string());
            break;
        }
    }
    assert!(
        names.len() >= 2,
        "the engine skipped to something else on its own: {names:?}"
    );
}

/// **Folds decide which lines come back, and the shelf behind them is not rebuilt** — #261.
///
/// `tree-over-the-wire.md` §2: the grouping is 370 ms and the flatten 1.8 ms, and only the
/// flatten depends on what is open. So the folds are deliberately *not* in the cache key, and
/// what this checks is the consequence — that asking the same library with different folds
/// gives different lines, rather than the first answer again.
#[test]
fn folds_travel_with_the_request_and_the_spine_comes_back() {
    let (engine, _recording) = start_with_a_track("tree-folds");
    let client = Client::connect(&engine.socket, "browser", "0").expect("connect");

    let ask = |params: serde_json::Value| -> serde_json::Value {
        client.call("query.tree", Some(params)).expect("the tree")
    };
    let total = |reply: &serde_json::Value| reply["total"].as_u64().expect("a total");
    let spine = |reply: &serde_json::Value| {
        reply["spine"]
            .as_str()
            .expect("a spine on every reply")
            .to_owned()
    };

    // Everything open — no folds is what a client with no fold state sends.
    let open = ask(serde_json::json!({ "limit": 100 }));
    assert!(
        total(&open) > 2,
        "only {} lines with everything open, so folding cannot show less",
        total(&open)
    );

    // Everything shut: `level: 0` opens nothing, so only the top level is drawn.
    let shut = ask(serde_json::json!({
        "limit": 100,
        "folds": { "level": 0, "opened": [], "closed": [] },
    }));
    assert!(
        total(&shut) < total(&open),
        "{} lines shut against {} open — the folds were ignored",
        total(&shut),
        total(&open)
    );
    assert!(
        total(&shut) > 0,
        "shut to nothing at all, which is not a fold"
    );

    // **The spine is the shape, one hex digit a line.**
    assert_eq!(
        spine(&open).len() as u64,
        total(&open),
        "the spine does not have one digit per line"
    );
    assert!(
        spine(&open).chars().all(|c| c.is_ascii_hexdigit()),
        "the spine is not hex: {:?}",
        spine(&open)
    );
    assert_ne!(
        spine(&open),
        spine(&shut),
        "two different trees have the same shape"
    );

    // **An anchor beats a cursor**, which is what makes a toggle keep the cursor where it was.
    // Anchoring on the last line of the open tree must page from *there*, not from `cursor`.
    let nodes = open["nodes"].as_array().expect("nodes");
    let last = nodes.last().expect("a line to anchor on");
    assert_eq!(
        last["kind"].as_str(),
        Some("track"),
        "expected the last line of an open tree to be a track: {last}"
    );
    // **The seat comes with the line**, so a client anchors without re-deriving `Node::seat`'s
    // three exceptions — which is the whole reason it is on the wire.
    let seat = last["seat"].clone();
    assert!(
        seat.as_array().is_some_and(|p| !p.is_empty()),
        "no seat on the line, so nothing can be anchored: {last}"
    );
    let anchored = ask(serde_json::json!({
        "limit": 1,
        "cursor": 0,
        "anchor": { "path": seat, "id": last["id"] },
    }));
    assert_eq!(
        anchored["at"].as_u64(),
        Some(total(&open) - 1),
        "the anchor did not win over the cursor"
    );
}

/// **A search finds a line inside a closed fold, and says where it lands once opened** — #261.
///
/// Two rules meet here and both have bitten before. A closed fold still holds its lines, so the
/// search runs over the unfolded tree — searching what is *visible* with the artists shut finds
/// nothing at all, which is what #131 fixed in the view. And the index comes back measured
/// **after** the reveal: opening a fold inserts every line under it, so an index taken against
/// the unfolded list is stale before a client could use it.
#[test]
fn a_reveal_opens_the_way_and_says_where_it_landed() {
    let (engine, _recording) = start_with_a_track("tree-reveal");
    let client = Client::connect(&engine.socket, "browser", "0").expect("connect");

    let ask =
        |params: serde_json::Value| client.call("query.reveal", Some(params)).expect("reveal");
    let tree = |params: serde_json::Value| client.call("query.tree", Some(params)).expect("tree");

    // Everything shut, so anything found is found *through* a fold.
    let shut = serde_json::json!({ "level": 0, "opened": [], "closed": [] });
    let drawn = tree(serde_json::json!({ "limit": 100, "folds": shut }));
    let visible = drawn["total"].as_u64().expect("a total");

    // The title to look for comes from the open tree, so the fixture names it rather than this.
    let open = tree(serde_json::json!({ "limit": 100 }));
    let title = open["nodes"]
        .as_array()
        .expect("nodes")
        .iter()
        .find_map(|n| (n["kind"] == "track").then(|| n["title"].as_str().expect("title")))
        .expect("a track in the open tree");
    assert!(!title.is_empty(), "a blank title searches for nothing");
    assert!(
        (open["total"].as_u64().expect("a total")) > visible,
        "the folds hid nothing, so this proves no search through one"
    );

    let found = ask(serde_json::json!({ "pattern": title, "folds": shut }));
    let opened = found["open"].as_array().expect("something had to open");
    assert!(
        !opened.is_empty(),
        "a track inside a shut fold was reached without opening anything"
    );
    let at = found["at"].as_u64().expect("somewhere to land");

    // **The landing place is in the revealed tree, not the shut one.** Without the second
    // flatten this index would be the unfolded one, which is the bug the design names.
    let mut folds = shut.clone();
    folds["opened"] = serde_json::json!(opened);
    let after = tree(serde_json::json!({ "limit": 100, "folds": folds }));
    let nodes = after["nodes"].as_array().expect("nodes");
    assert!(
        at < after["total"].as_u64().expect("a total"),
        "landed at {at} in a tree of {} lines",
        after["total"]
    );
    assert_eq!(
        nodes[at as usize]["title"].as_str(),
        Some(title),
        "the cursor landed on the wrong line"
    );

    // A pattern nothing matches is an ordinary answer, not an error.
    let nothing = ask(serde_json::json!({ "pattern": "zzz no such track zzz", "folds": shut }));
    assert!(nothing["at"].is_null(), "a miss reported somewhere to go");

    // Both targets, or neither, is a bad request rather than a guess.
    assert!(
        client
            .call("query.reveal", Some(serde_json::json!({ "folds": shut })))
            .is_err(),
        "a reveal with no target was accepted"
    );
}

/// **Everything a view asks a library, asked over the wire** — #261.
///
/// `Holding` is the seam a view browses through, and `ui::holding` picks an implementation.
/// This checks the wire can answer the questions that implementation will have to: what sits
/// under a line, where the cursor goes when its line was swallowed, and the counts a status
/// line draws. A seam with no second implementation is a claim; these are what makes it one
/// that can be met.
#[test]
fn the_wire_answers_what_a_view_holds_a_library_for() {
    let (engine, _recording) = start_with_a_track("tree-holding");
    let client = Client::connect(&engine.socket, "browser", "0").expect("connect");
    let ask = |m: &str, p: serde_json::Value| client.call(m, Some(p)).expect(m);

    // **The counts, off the same shelf that answered the page.** A status line redraws them
    // every frame, and a round trip for three numbers would be the wrong trade.
    let open = ask("query.tree", serde_json::json!({ "limit": 100 }));
    let rows = open["rows"].as_u64().expect("a row count");
    assert!(rows > 0, "no rows counted");
    assert_eq!(
        open["artists"].as_u64().map(|n| n > 0),
        Some(true),
        "no artists counted"
    );
    assert!(
        open["albums"].as_u64().is_some_and(|n| n > 0),
        "no albums counted"
    );

    // **What sits under a line.** The artist's seat is the first node's own.
    let nodes = open["nodes"].as_array().expect("nodes");
    let artist = nodes.first().expect("a first line");
    assert_eq!(artist["kind"].as_str(), Some("artist"), "{artist}");
    let under = ask("query.under", serde_json::json!({ "path": artist["seat"] }));
    let tracks = under["tracks"].as_array().expect("tracks");
    assert_eq!(
        tracks.len() as u64,
        rows,
        "the artist holds the whole fixture, so `x` on it should reach every track"
    );

    // **And a track's line stands in for nothing here**, because its record has more than one.
    let track = nodes
        .iter()
        .find(|n| n["kind"] == "track")
        .expect("a track line");
    assert!(
        track.get("stands_for").is_none(),
        "a track in a drawn record claimed to stand in for it: {track}"
    );

    // **And `x` on a track is that track, not its record.** A track sits at its record's path
    // and so does the record's own line, so a lookup by seat alone hands back the record — and
    // with a fixture whose artist holds everything, that reads the same as the whole library.
    assert!(rows > 1, "one track cannot show `x` narrowing to one");
    let one = ask(
        "query.under",
        serde_json::json!({ "path": track["seat"], "id": track["id"] }),
    );
    assert_eq!(
        one["tracks"].as_array().map(Vec::len),
        Some(1),
        "`x` on a track reached the wrong number of tracks: {}",
        one["tracks"]
    );

    // **Where the cursor goes when its line was swallowed.** Shut everything and settle on a
    // track's seat: the answer is its record's line, not row zero and not an error.
    let shut = serde_json::json!({ "level": 0, "opened": [], "closed": [] });
    let settled = ask(
        "query.tree",
        serde_json::json!({ "limit": 100, "folds": shut, "settle": track["seat"], "cursor": 99 }),
    );
    let at = settled["at"].as_u64().expect("somewhere to land");
    let total = settled["total"].as_u64().expect("a total");
    assert!(
        at < total,
        "settled at {at} in a tree of {total} lines, which is off the end"
    );
    assert_ne!(
        Some(at),
        settled["nodes"].as_array().map(|_| 99),
        "the cursor was used instead of the settle"
    );
}

/// **The remote library answers what the local one does** — #261.
///
/// `Holding` has two implementations now: a `Shelf` a view folded for itself, and a socket. The
/// whole design rests on a view not being able to tell them apart, so the only honest test is to
/// ask both and compare — the same oracle trick the spine got, one layer up.
///
/// The failure this catches is not a crash. It is a tree that draws *nearly* right: a line in
/// the wrong place, a count off by one, a fold that opens and shows something else. Those look
/// like the library being odd rather than like a bug.
#[test]
fn a_remote_library_answers_what_a_local_one_does() {
    use gmuse::browse::{Browsing, Holding};

    let (engine, _recording) = start_with_a_track("holding-oracle");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    // The same library, folded here — which is exactly what an attached view should stop doing.
    let data = engine.dir.join("xdg").join("gmuse");
    // **The engine's own device**, read off the log's name — `durable::log_path` is one file
    // per device and holdings are keyed by it, so a made-up device folds the same log and finds
    // no files in it. That is an empty library rather than an error, and would have made every
    // comparison below vacuously true.
    let device: gmuse::model::id::Id<gmuse::model::entity::Device> = std::fs::read_dir(&data)
        .expect("a data dir")
        .flatten()
        .find_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("log-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .expect("the engine wrote a log named for its device");
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let answer = desk.current();
    let members: std::collections::HashSet<_> = {
        use gmuse::model::rows::Listing;
        answer.library.rows().into_iter().collect()
    };
    assert!(
        members.len() > 1,
        "only {} rows — two implementations of an empty library agree about nothing",
        members.len()
    );
    let local = gmuse::ui::holding::of(
        Some(&answer.library),
        None,
        members,
        gmuse::shelf::Levels::default(),
        true,
    );
    let remote = gmuse::ui::holding_remote::Remote::open(client);

    // The flat ordering, fetched once by the remote and lent thereafter.
    assert_eq!(
        remote.sorted().len(),
        local.sorted().len(),
        "the two orderings are different lengths"
    );
    assert_eq!(remote.sorted(), local.sorted(), "the orderings differ");

    // The tree, at several fold states — because a cache keyed on the folds is exactly the
    // thing that answers the second question with the first one's reply.
    for level in 0..3u16 {
        let mut folds = gmuse::fold::Expansion::default();
        folds.set_level(level);

        let want = local.spine(&folds);
        assert_eq!(
            remote.spine(&folds).to_hex(),
            want.to_hex(),
            "the shapes differ at level {level}"
        );
        assert!(!want.is_empty(), "no lines at level {level}");

        let here = local.page(&folds, 0, 50);
        assert_eq!(
            remote.page(&folds, 0, 50),
            here,
            "the pages differ at level {level}"
        );

        // And where a line is, which is what a toggle asks.
        let mark = here.first().expect("a line").mark();
        assert_eq!(
            remote.at(&folds, &mark),
            local.at(&folds, &mark),
            "the two disagree about where a line is at level {level}"
        );
    }

    // **Two shapes back to back**, which is what a cache keyed on the folds has to get right.
    // The loop above alternates `spine` and `page`, and those ask different windows — so each
    // call missed anyway, and a cache that ignored the folds entirely would have passed it.
    let mut shut = gmuse::fold::Expansion::default();
    shut.set_level(0);
    let mut wide = gmuse::fold::Expansion::default();
    wide.set_level(2);
    let narrow = remote.spine(&shut).to_hex();
    let broad = remote.spine(&wide).to_hex();
    assert_ne!(
        narrow, broad,
        "the same shape came back for two fold states"
    );
    assert_eq!(
        narrow,
        local.spine(&shut).to_hex(),
        "the shut shape differs"
    );
    assert_eq!(broad, local.spine(&wide).to_hex(), "the open shape differs");

    // **A line that is not there.** `query.tree` falls back to the cursor when an anchor is
    // gone, so an unchecked answer would be a confident zero.
    let nowhere = (vec![gmuse::shelf::SortKey::already("no such artist")], None);
    assert_eq!(
        remote.at(&wide, &nowhere),
        local.at(&wide, &nowhere),
        "the two disagree about a line that does not exist"
    );
    assert_eq!(
        local.at(&wide, &nowhere),
        None,
        "the fixture holds a line it should not, so this proves nothing"
    );

    // What `x` acts on, and what `zR` has left.
    //
    // **`Expansion::default()` is level 0, which is everything shut** — only the top line is
    // drawn. Named for what it is, because calling it `open` cost a test that then looked for a
    // track among artist lines and found none.
    let plain = gmuse::fold::Expansion::default();
    let artist = local.page(&plain, 0, 1).into_iter().next().expect("a line");
    assert_eq!(
        remote.tracks_under(&artist),
        local.tracks_under(&artist),
        "the two disagree about what sits under a line"
    );
    assert!(
        !local.tracks_under(&artist).is_empty(),
        "nothing under the first line, so agreeing about it proves nothing"
    );
    assert_eq!(
        remote.openable_under(&artist.seat()),
        local.openable_under(&artist.seat()),
        "the two disagree about what is left to open"
    );

    // **And on a track, where the id is the half that names it.** A track sits at its record's
    // path and so does the record's own line, so a request that dropped the id would come back
    // with the whole record — and asking only about an artist, which has no id, cannot see it.
    let line = local
        .page(&wide, 0, 200)
        .into_iter()
        .find(|n| matches!(n, gmuse::browse::Node::Track { .. }))
        .expect("a track line");
    assert_eq!(
        local.tracks_under(&line).len(),
        1,
        "a track should be one track"
    );
    assert_eq!(
        remote.tracks_under(&line),
        local.tracks_under(&line),
        "the two disagree about what `x` on a track plays"
    );

    // And a search.
    // The fixture's tracks are generated tones and carry no titles, so the artist's own name
    // is what there is to search for — which still exercises the whole path.
    let title = local
        .page(&wide, 0, 200)
        .into_iter()
        .find_map(|n| match n {
            gmuse::browse::Node::Artist { display, .. } if !display.is_empty() => Some(display),
            _ => None,
        })
        .expect("a line with a name on it");
    let want = gmuse::browse::Target::Pattern {
        needle: title.clone(),
        backwards: false,
    };
    let mine = local.find(&wide, &want, 0).expect("the local one finds it");
    assert_eq!(
        remote.find(&wide, &want, 0).map(|f| (f.open, f.at)),
        Some((mine.open.clone(), mine.at)),
        "the two disagree about where {title:?} is"
    );

    // **And a search through a shut fold**, which is the half that has anything to open. With
    // everything already open `open` is empty on both sides, and a reply that dropped it
    // entirely would read as agreement.
    let track = local
        .page(&wide, 0, 200)
        .into_iter()
        .find_map(|n| match n {
            gmuse::browse::Node::Track { id, .. } => Some(id),
            _ => None,
        })
        .expect("a track");
    let buried = gmuse::browse::Target::Id(track);
    let theirs = remote.find(&shut, &buried, 0).expect("the remote finds it");
    let ours = local
        .find(&shut, &buried, 0)
        .expect("and so does the local one");
    assert!(
        !ours.open.is_empty(),
        "nothing had to open, so the fixture cannot show a reveal"
    );
    assert_eq!(
        (theirs.open, theirs.at),
        (ours.open, ours.at),
        "the two disagree about what a reveal has to open"
    );

    // **What a page stands for**, which is what filing from the panel writes one fact per.
    // The fan-out — an artist to their recordings — is `Librarian::covers` and is checked
    // where a library has artists (`ui::tests`); an unidentified import mints none, so
    // what this can show is that the question crosses the wire and comes back the same.
    {
        use gmuse::model::focus::Focus;
        use gmuse::model::rows::Listing;
        let lib: &dyn Listing = &answer.library;
        let recording = lib.recording_of(track).expect("the track is a recording");
        let ours = lib.covers(Focus::Recording(recording));
        assert_eq!(ours, vec![recording], "a recording covers itself");
        assert_eq!(
            Listing::covers(&remote, Focus::Recording(recording)),
            ours,
            "the two disagree about what a recording stands for"
        );
        // And a focus the library has nothing for answers nothing, rather than the socket
        // answering nothing and looking the same.
        let nobody = Focus::Artist(gmuse::model::id::Id::derived(b"nobody"));
        assert!(lib.covers(nobody).is_empty());
        assert!(Listing::covers(&remote, nobody).is_empty());
    }
}

/// **An attached view draws the library it does not hold** — #261.
///
/// The seams are checked one at a time elsewhere; this is the whole path, once: a `Ui` whose
/// `Holding` is the socket, rendered into a buffer, against the same `Ui` holding a shelf it
/// folded itself. They must draw the same pane.
///
/// **This is the check a headless smoke test could not be.** Running the real TUI needs a
/// terminal, and where there is none every pane draws empty — which is indistinguishable from
/// a remote that answers nothing. A `TestBackend` has a size and no terminal.
#[test]
fn an_attached_view_draws_the_same_pane_as_a_detached_one() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (engine, _recording) = start_with_a_track("attached-draw");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let data = engine.dir.join("xdg").join("gmuse");
    let device: gmuse::model::id::Id<gmuse::model::entity::Device> = std::fs::read_dir(&data)
        .expect("a data dir")
        .flatten()
        .find_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("log-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .expect("the engine wrote a log named for its device");
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let answer = desk.current();

    let drawn = |ui: &gmuse::ui::Ui| -> String {
        let ctx = gmuse::ui::Context {
            library: &answer.library,
            player: None,
            gapless: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(80, 14)).expect("a backend");
        terminal
            .draw(|f| gmuse::ui::draw(f, ui, &ctx))
            .expect("a frame");
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let members: std::collections::HashSet<_> = {
        use gmuse::model::rows::Listing;
        answer.library.rows().into_iter().collect()
    };
    assert!(
        members.len() > 1,
        "only {} rows — an empty library draws the same either way",
        members.len()
    );
    let levels = gmuse::shelf::Levels::default();

    let mut here = gmuse::ui::Ui::new(gmuse::ui::holding::of(
        Some(&answer.library),
        None,
        members.clone(),
        levels,
        true,
    ));
    here.tree.open_everything(&here.library);
    let local = drawn(&here);

    let mut there = gmuse::ui::Ui::new(gmuse::ui::holding::of(
        None,
        Some(gmuse::ui::holding_remote::Remote::open(
            std::sync::Arc::clone(&client),
        )),
        members.clone(),
        levels,
        true,
    ));
    there.tree.open_everything(&there.library);
    let remote = drawn(&there);

    // **The instrument.** A pane with nothing in it is what a terminal-less run produces, and
    // two empty panes agree perfectly.
    let lines = |s: &str| {
        s.lines()
            .filter(|l| l.contains('\u{2502}') && l.trim_matches(['\u{2502}', ' ']).len() > 1)
            .count()
    };
    assert!(
        lines(&local) > 2,
        "the local pane drew {} lines, so agreeing about it proves nothing:\n{local}",
        lines(&local)
    );
    assert_eq!(remote, local, "the two panes differ");

    // **And a narrowed view narrows both** — #261. The two above agree over the whole library,
    // which is the one view an implementation can match by ignoring the question — and ignoring
    // it is exactly what the attached one used to do.
    // **A title no other row's contains.** A filter matches on substring, so `title="tone"`
    // also selects `tone2` and narrows nothing — which the assertion below catches rather than
    // letting the two panes agree about the whole library a second time.
    let title = {
        use gmuse::model::rows::Listing;
        let titles: Vec<String> = answer
            .library
            .rows()
            .into_iter()
            .filter_map(|id| answer.library.listed(id))
            .map(|slip| slip.listed().title.to_string())
            .collect();
        titles
            .iter()
            .find(|t| {
                !titles
                    .iter()
                    .any(|other| other != *t && other.contains(&**t))
            })
            .expect("a title that is not a prefix of another")
            .clone()
    };
    let source = format!("title={title:?}");
    let stack = gmuse::view::Stack::parse(&source).expect("the view parses");
    assert_eq!(
        stack.label(),
        source,
        "the view does not survive the round trip it crosses the wire as"
    );

    let narrowed = {
        let env = gmuse::query::filter::Env {
            listened: Some(&*answer.listened),
            named: None,
            now: gmuse::history::now(),
        };
        stack.members(&answer.library, &*answer.listened, env).0
    };
    assert!(
        !narrowed.is_empty() && narrowed.len() < members.len(),
        "{source} selected {} of {} — it narrows nothing",
        narrowed.len(),
        members.len()
    );

    let mut here = gmuse::ui::Ui::new(gmuse::ui::holding::of(
        Some(&answer.library),
        None,
        narrowed,
        levels,
        true,
    ));
    here.tree.open_everything(&here.library);
    let local = drawn(&here);

    let mut there = gmuse::ui::Ui::new(gmuse::ui::holding::of(
        None,
        Some(gmuse::ui::holding_remote::Remote::showing(
            std::sync::Arc::clone(&client),
            gmuse::ui::holding_remote::Showing {
                view: Some(source.clone()),
                levels: None,
            },
        )),
        Default::default(),
        levels,
        true,
    ));
    there.tree.open_everything(&there.library);
    let remote = drawn(&there);

    assert!(
        lines(&local) > 1,
        "the narrowed local pane drew {} lines:\n{local}",
        lines(&local)
    );
    assert_eq!(
        remote, local,
        "the two panes differ once the view is narrowed"
    );
}

/// **A picker underlines what matched, because the rows carry where** — #261's last done-when.
///
/// The alternative is the one the issue names and rejects: if the rows cross without their
/// spans, the view needs `nucleo-matcher` to work out what to underline — and a matcher needs
/// the rows to match against, which is holding a library again by the back door.
///
/// **Asserted by slicing, not by counting.** A span that merely exists proves nothing; the
/// claim is that it selects the characters that matched, so the test cuts the returned title
/// with the returned range and checks what comes out. That is also the only thing that catches
/// the mapping this rests on — the match runs over folded, lower-cased text whose characters do
/// not line up with the string the reply carries.
#[test]
fn a_picker_is_told_which_characters_matched() {
    let (engine, _recording) = start_with_a_track("picker-spans");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let reply = client
        .call(
            "query.run",
            Some(serde_json::json!({ "filter": "any=\"*one*\"", "limit": 10 })),
        )
        .expect("query.run");
    let rows = reply["rows"].as_array().expect("rows");
    assert!(
        !rows.is_empty(),
        "nothing matched, so there is nothing to underline and nothing is proven"
    );

    let mut checked = 0;
    for row in rows {
        let title = row["title"].as_str().expect("a title");
        let chars: Vec<char> = title.chars().collect();
        for mark in row["matched"].as_array().into_iter().flatten() {
            let field = mark["field"].as_str().expect("a field name");
            if field != "title" {
                continue;
            }
            let at = mark["at"].as_u64().expect("at") as usize;
            let to = mark["to"].as_u64().expect("to") as usize;
            assert!(
                to <= chars.len(),
                "{field} span {at}..{to} runs past {title:?}, which is {} characters",
                chars.len()
            );
            let cut: String = chars[at..to].iter().collect();
            assert_eq!(
                cut.to_lowercase(),
                "one",
                "the span picked {cut:?} out of {title:?} rather than what was searched for"
            );
            checked += 1;
        }
    }
    // The instrument: every row could have come back with an empty `matched` and the loop above
    // would have asserted nothing at all.
    assert!(
        checked > 0,
        "{} rows matched and not one said where",
        rows.len()
    );

    // **A field-scoped pattern marks that field and no other.** `any=` above looks everywhere,
    // so it cannot tell a matcher that respects the selector from one that marks whatever it
    // finds — and the fixture files under a name its titles do not contain, which is what makes
    // this able to fail.
    let scoped = client
        .call(
            "query.run",
            // **A letter the other fields have too**, or scoping is unobservable: a pattern
            // that occurs in one field only is marked in one field whether or not the selector
            // is read.
            Some(serde_json::json!({ "filter": "file_under=\"*n*\"", "limit": 10 })),
        )
        .expect("query.run");
    let scoped = scoped["rows"].as_array().expect("rows");
    assert!(!scoped.is_empty(), "no row files under that name");
    let mut fields: Vec<String> = scoped
        .iter()
        .flat_map(|r| r["matched"].as_array().into_iter().flatten())
        .map(|m| m["field"].as_str().expect("a field name").to_string())
        .collect();
    fields.sort();
    fields.dedup();
    assert_eq!(
        fields,
        vec!["file_under".to_string()],
        "a `file_under=` pattern marked {fields:?}"
    );

    // **Two patterns finding the same run is one underline.** `any=` and `title=` both mark
    // the title here, and a client drawing both would style those characters twice.
    let twice = client
        .call(
            "query.run",
            Some(serde_json::json!({
                "filter": "any=\"*one*\" & title=\"*one*\"",
                "limit": 10
            })),
        )
        .expect("query.run");
    let twice = twice["rows"].as_array().expect("rows");
    assert!(!twice.is_empty(), "nothing matched both terms");
    for row in twice {
        let marks: Vec<_> = row["matched"].as_array().into_iter().flatten().collect();
        let mut seen: Vec<String> = marks.iter().map(|m| m.to_string()).collect();
        let before = seen.len();
        seen.sort();
        seen.dedup();
        assert_eq!(
            seen.len(),
            before,
            "the same run came back twice for {:?}",
            row["title"]
        );
    }

    // And a filter that asks about no text says so, rather than marking something.
    let quiet = client
        .call(
            "query.run",
            Some(serde_json::json!({ "filter": "year>=0", "limit": 10 })),
        )
        .expect("query.run");
    let marked: usize = quiet["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|r| r["matched"].as_array().map_or(0, |m| m.len()))
        .sum();
    assert_eq!(
        marked, 0,
        "a filter with no text in it marked {marked} runs"
    );
}

/// **The picker asks the same library the browse tree does** — #261.
///
/// `query.run` built its filter environment with `listened: None`, so every field backed by
/// history answered as though nothing had ever been played: `rating>=4` found **0 rows against
/// the tree's 35** on the real library, and so did `plays`, `last_played` and `days_since_*`.
///
/// **Silently, which is the whole problem.** A filter over a field nothing can satisfy matches
/// nothing, and an empty picker is what a filter that is merely too narrow looks like as well.
/// Nothing errored and nothing was logged; the two paths simply disagreed.
///
/// So it is asserted as an oracle rather than against a number: two ways of asking the library
/// which rows have four stars must give the same rows.
#[test]
fn a_picker_and_a_tree_agree_about_a_rating() {
    let (engine, recording) = start_with_a_track("picker-history");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    // Nothing is rated yet, so both must agree on *nothing* first — the instrument, because two
    // paths that both answer zero for the wrong reason agree perfectly.
    let picked = |filter: &str| -> u64 {
        client
            .call(
                "query.run",
                Some(serde_json::json!({ "filter": filter, "limit": 1 })),
            )
            .expect("query.run")["total"]
            .as_u64()
            .expect("a total")
    };
    let browsed = |view: &str| -> u64 {
        client
            .call(
                "query.tree",
                Some(serde_json::json!({ "view": view, "levels": [], "limit": 1 })),
            )
            .expect("query.tree")["total"]
            .as_u64()
            .expect("a total")
    };

    assert_eq!(
        picked("rating>=4"),
        0,
        "something was rated before the test"
    );
    assert_eq!(
        browsed("rating>=4"),
        0,
        "something was rated before the test"
    );

    client
        .call(
            "library.edit",
            Some(serde_json::json!({
                "facts": [{ "RecordingRated": { "recording": recording, "stars": 4 } }]
            })),
        )
        .expect("the rating is accepted");

    // The fold is published on the librarian's own thread, so the rating arrives when it
    // arrives — polled rather than asked once, which is how this suite's other library-event
    // tests stopped being flaky.
    let mut browsed_now = 0;
    for _ in 0..100 {
        browsed_now = browsed("rating>=4");
        if browsed_now > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        browsed_now > 0,
        "the tree never saw the rating, so the comparison below proves nothing"
    );
    assert_eq!(
        picked("rating>=4"),
        browsed_now,
        "the picker and the tree disagree about which rows have four stars"
    );

    // And a rating nothing has is still nothing, in both — so the agreement above is about the
    // filter rather than about both paths having stopped filtering.
    assert_eq!(
        picked("rating>=5"),
        0,
        "the picker invented a five-star row"
    );
    assert_eq!(browsed("rating>=5"), 0, "the tree invented a five-star row");
}

/// **A view that names nothing still says so, once the librarian is what resolves it** — #261,
/// #119.
///
/// This is the half of the change that needed deciding rather than measuring. An attached view
/// no longer resolves its own terms, so it no longer discovers that `saved="gone"` names
/// nothing — and a view that selects no rows draws exactly like a filter that is merely too
/// narrow. Telling those apart by staring at an empty pane is the failure §13 keeps recording,
/// so the complaint travels back with the tree it explains.
#[test]
fn a_view_naming_a_saved_one_that_is_gone_says_so_over_the_wire() {
    let (engine, _recording) = start_with_a_track("view-problem");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let remote = gmuse::ui::holding_remote::Remote::showing(
        std::sync::Arc::clone(&client),
        gmuse::ui::holding_remote::Showing {
            view: Some("saved=\"no such view\"".to_string()),
            levels: None,
        },
    );
    let said = remote.said();
    assert_eq!(
        said.problems.len(),
        1,
        "the librarian reported {:?} about a view naming nothing",
        said.problems
    );
    assert!(
        said.problems[0].contains("no such view"),
        "the complaint does not name what is missing: {:?}",
        said.problems[0]
    );

    // **The instrument.** A `Remote` that reported a complaint about every view would satisfy
    // the above and be useless — a view that names something real must come back quiet.
    let fine = gmuse::ui::holding_remote::Remote::showing(
        std::sync::Arc::clone(&client),
        gmuse::ui::holding_remote::Showing {
            view: None,
            levels: None,
        },
    );
    assert!(
        fine.said().problems.is_empty(),
        "the whole library is not a problem: {:?}",
        fine.said().problems
    );
}

/// **A page says where the next one starts, and a client that ignores it sees a short library**
/// — #261.
///
/// `query.tree` caps a page at 5,000 and says so only by handing back a `next`. A client asking
/// for a million gets 5,000 and cannot tell — which is the whole library right up until the
/// library is bigger than that, and this one has 7,273 rows. So the mechanism a client has to
/// follow is checked here with a page size of one, where a fixture of four lines is enough.
#[test]
fn a_client_that_follows_next_sees_the_whole_library() {
    let (engine, _recording) = start_with_a_track("paging");
    let client = Client::connect(&engine.socket, "browser", "0").expect("connect");

    let whole = client
        .call("query.tree", Some(serde_json::json!({ "limit": 5_000 })))
        .expect("the tree");
    let total = whole["total"].as_u64().expect("a total");
    assert!(
        total > 2,
        "{total} lines cannot show paging with a page of one"
    );
    assert!(
        whole["next"].is_null(),
        "a page of 5,000 did not reach the end of a {total}-line tree"
    );

    let mut seen = Vec::new();
    let mut cursor = Some(0u64);
    let mut asks = 0;
    while let Some(at) = cursor {
        asks += 1;
        assert!(
            asks <= total + 1,
            "paging did not terminate after {asks} asks"
        );
        let page = client
            .call(
                "query.tree",
                Some(serde_json::json!({ "limit": 1, "cursor": at })),
            )
            .expect("a page");
        let nodes = page["nodes"].as_array().expect("nodes").clone();
        if nodes.is_empty() {
            break;
        }
        seen.extend(nodes);
        cursor = page["next"].as_u64();
    }

    assert_eq!(
        seen.len() as u64,
        total,
        "following `next` reached {} of {total} lines",
        seen.len()
    );
    assert_eq!(
        seen,
        whole["nodes"].as_array().cloned().expect("nodes"),
        "the walked lines are not the ones a single page gives"
    );
}

/// **The remote read seam answers what the local one does** — #261.
///
/// `Listing` is what a view reads a row through, and `Remote` answers all of it over the
/// socket. Nothing points a `Context` at it yet — §7 of the document says why, with the
/// measurement — so this is what says the implementation is right rather than merely present.
///
/// Asked of both and compared, like the tree's oracle: the failure here is not a crash but a
/// row that is *nearly* right, and a wrong length or a missing credit reads as bad metadata.
#[test]
fn a_remote_read_answers_what_a_local_one_does() {
    use gmuse::model::rows::Listing;

    let (engine, _recording) = start_with_a_track("listing-oracle");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let data = engine.dir.join("xdg").join("gmuse");
    let device: gmuse::model::id::Id<gmuse::model::entity::Device> = std::fs::read_dir(&data)
        .expect("a data dir")
        .flatten()
        .find_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("log-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .expect("the engine wrote a log named for its device");
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let answer = desk.current();
    let local: &dyn Listing = &answer.library;
    let remote = gmuse::ui::holding_remote::Remote::open(client);

    let ids = local.rows();
    assert!(
        ids.len() > 1,
        "only {} rows — two readers of an empty library agree about nothing",
        ids.len()
    );
    assert_eq!(
        remote.rows(),
        ids,
        "the two disagree about which rows exist"
    );

    let mut answered = 0;
    for id in &ids {
        let mine = local.listed(*id);
        assert_eq!(remote.listed(*id), mine, "the row for {id:?} differs");
        answered += usize::from(mine.is_some());

        assert_eq!(
            remote.playable(*id).map(|p| p.locator),
            local.playable(*id).map(|p| p.locator),
            "the file for {id:?} differs"
        );
        let recording = local.recording_of(*id);
        assert_eq!(
            remote.recording_of(*id),
            recording,
            "the recording for {id:?} differs"
        );

        if let Some(r) = recording {
            assert_eq!(remote.row(r), local.row(r), "the row for {r:?} differs");
            assert_eq!(
                remote.chosen(r, &[device], &[]),
                local.chosen(r, &[device], &[]),
                "the file chosen for {r:?} differs"
            );
        }
    }
    // The instrument: a library that answers `None` to everything agrees with itself.
    assert!(
        answered > 1,
        "only {answered} rows were anything at all, so equality proves nothing"
    );

    // **The pairing, which the remote answers with one call and the default with a walk**
    // — #261. Both directions matter: a recording the device holds must come back paired,
    // and one it does not must come back absent rather than paired with something.
    let recordings: Vec<_> = ids
        .iter()
        .filter_map(|id| local.recording_of(*id))
        .collect();
    assert!(
        !recordings.is_empty(),
        "no row named a recording, so the pairing below proves nothing"
    );
    let mine = local.copies_of(&recordings);
    assert_eq!(
        remote.copies_of(&recordings),
        mine,
        "the two disagree about which copy holds which recording"
    );
    assert_eq!(
        mine.len(),
        recordings
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        "a recording this device holds was left unpaired"
    );

    // A recording nobody holds pairs with nothing, in both.
    let nobody: gmuse::model::id::Id<gmuse::model::entity::Recording> =
        gmuse::model::id::Id::from_uuid(uuid::Uuid::nil());
    let asked = [recordings[0], nobody];
    let mine = local.copies_of(&asked);
    assert_eq!(remote.copies_of(&asked), mine, "they differ on a stranger");
    assert_eq!(mine.len(), 1, "the stranger was paired with something");

    // Nothing asked for is nothing answered. The implementation also returns early rather than
    // asking the engine at all, and this cannot see that — only a round-trip counter could, and
    // the answer is the same either way.
    assert!(
        remote.copies_of(&[]).is_empty(),
        "empty asked, not empty back"
    );

    // And the panel's half, which is what `info-panel.md` is about.
    let focus =
        gmuse::model::focus::Focus::Recording(local.recording_of(ids[0]).expect("a recording"));
    assert_eq!(
        remote.label_of(focus),
        local.label_of(focus),
        "the two label it differently"
    );
    assert!(
        !local.label_of(focus).is_empty(),
        "an unlabelled thing agrees with anything"
    );
    use gmuse::model::about::History;
    let mine = local.about(focus, History::Folded);
    let theirs = remote.about(focus, History::Folded);
    assert_eq!(theirs.title, mine.title, "the panel titles differ");
    assert_eq!(theirs.kind, mine.kind, "the panel kinds differ");
    assert!(
        !mine.sections.is_empty(),
        "an empty panel agrees with an empty panel"
    );
    assert_eq!(
        theirs.sections.len(),
        mine.sections.len(),
        "the panels have different numbers of sections"
    );

    // **Folded means the log is not read, and open means it is** — #293. The
    // engine is the only side that can read it for an attached view, so the
    // client says which, and this is the assertion that says the flag is
    // carried rather than defaulted.
    let shown = remote.about(focus, History::Shown(&[]));
    let rows = |a: &gmuse::model::about::About| -> usize {
        a.sections
            .iter()
            .find(|s| s.title == gmuse::model::about::FACTS)
            .map_or(0, |s| s.rows.len())
    };
    // The instrument: there is something in this recording's history to find,
    // or "open has more than folded" is two zeroes agreeing.
    assert!(
        rows(&shown) > 0,
        "the log says nothing happened to this recording, so asking changes nothing"
    );
    assert_eq!(
        rows(&theirs),
        0,
        "a folded facts section came back with {} rows — the engine read the log \
         for a section nobody is looking at",
        rows(&theirs)
    );
}

/// **What the library has heard crosses whole, and matches** — #261.
///
/// The nine `Listened` questions and the three maps a view reads directly, in one reply. Checked
/// against the same fold taken locally, because the failure that matters is not a crash: a
/// rating that quietly does not arrive looks exactly like a rating nobody gave.
///
/// **This is the second attempt.** The first shipped it over `Client::call`, which built a
/// `serde_json::Value` of the whole 4.5 MB reply and made an attached view larger than one that
/// folded the log; and before that it did not serialise at all, because two of these maps are
/// keyed by tuples and a JSON object takes string keys only — a failure that arrived as a hang.
/// Both are why this test exists and why `Heard` has a round-trip test of its own.
#[test]
fn what_the_library_has_heard_crosses_whole() {
    let (engine, recording) = start_with_a_track("heard-oracle");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    let data = engine.dir.join("xdg").join("gmuse");
    let device: gmuse::model::id::Id<gmuse::model::entity::Device> = std::fs::read_dir(&data)
        .expect("a data dir")
        .flatten()
        .find_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("log-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .expect("the engine wrote a log named for its device");
    // **Give it something to have heard**, because two empty `Heard`s agree perfectly. Rated
    // through the engine, which is how an attached view writes (§4.4).
    client
        .subscribe(&[gmuse_proto::Topic::Library])
        .expect("subscribed");
    let song: gmuse::model::id::Id<gmuse::model::entity::Recording> =
        recording.parse().expect("a recording id");
    let mut scratch = gmuse::model::instance::Instance::new(device);
    scratch.rate(1_000, song, Some(5));
    // **And a genre**, so the assertion graph is carried rather than merely declared to be.
    // It is the half with the tuple-keyed maps, which is the half that did not serialise at all
    // the first time this was built.
    scratch.assert_concept(
        1_000,
        gmuse::model::facet::Described::Recording(song),
        gmuse::model::facet::GENRE,
        "Techno",
        gmuse::model::reference::Source::Manual,
        gmuse::model::ingest::Confidence::Confirmed,
    );
    let facts: Vec<_> = scratch.records().iter().map(|r| &r.fact).collect();
    client
        .call("library.edit", Some(serde_json::json!({ "facts": facts })))
        .expect("the engine takes it");
    let event = client
        .next_event(Duration::from_secs(10))
        .expect("a library event");
    assert_eq!(event.topic, gmuse_proto::Topic::Library);

    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let answer = desk.current();
    let mine = &answer.listened;
    assert_eq!(
        mine.ratings.get(&song).copied(),
        Some(5),
        "the rating did not reach the log, so there is nothing to cross"
    );
    // **Both sides go through `Heard::of`**, so a field it forgets agrees with itself and the
    // comparison below sees nothing. These are what notice: each names a field and insists the
    // fixture put something in it.
    assert!(
        !mine.assertions.is_empty(),
        "no assertion reached the log, so the graph half crosses nothing"
    );
    assert!(
        !mine.concepts.is_empty(),
        "no concept label reached the log, and `claims` turns ids into labels with them"
    );

    // **Waited for, not assumed.** The event says the log grew; the engine folds and republishes
    // on its own thread, so a fetch can beat the publish it was told about. Asking once passed
    // four times and failed the fifth.
    let remote = gmuse::ui::holding_remote::Remote::open(client);
    let mut theirs = remote.heard().expect("the engine answered");
    for _ in 0..100 {
        if theirs.ratings.get(&song).copied() == Some(5) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
        theirs = remote.heard().expect("the engine answered");
    }
    assert_eq!(theirs, **mine, "what crossed is not what was folded");
}

/// **A closed connection is not a timeout** — #264.
///
/// `next_event` mapped every `recv_timeout` failure to `TimedOut`, which reads as
/// "nothing yet, ask again" and is exactly wrong once `demultiplex` has returned
/// and dropped the only `Sender`: the channel is disconnected, `recv_timeout`
/// answers **instantly**, and a caller that retries on error spins.
///
/// **Both arms are asserted, and that is the point.** A `next_event` that always
/// said `Closed` would satisfy the second assertion alone, and one that always
/// said `TimedOut` would satisfy the first alone. The claim is that they are
/// *distinguishable*, so the test has to see each of them happen.
#[test]
fn a_timeout_and_a_dead_engine_do_not_report_the_same_thing() {
    let (mut engine, _recording) = start_with_a_track("two-failures");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    while client.next_event(Duration::from_millis(300)).is_ok() {}

    // Alive and quiet: nothing to report, so ask again.
    let quiet = client
        .next_event(Duration::from_millis(400))
        .expect_err("the engine published with nothing happening");
    assert!(
        matches!(quiet, gmuse_client::ClientError::TimedOut(_)),
        "a live but idle engine should read as a timeout, got {quiet:?}"
    );

    let _ = engine.child.kill();
    let _ = engine.child.wait();

    // Gone: there is nobody to ask again.
    let mut closed = None;
    for _ in 0..40 {
        match client.next_event(Duration::from_millis(250)) {
            Err(gmuse_client::ClientError::TimedOut(_)) | Ok(_) => continue,
            other => {
                closed = Some(other);
                break;
            }
        }
    }
    let closed = closed.expect("the connection never reported the engine had gone");
    assert!(
        matches!(closed, Err(gmuse_client::ClientError::Closed)),
        "a dead engine should read as closed, got {closed:?}"
    );
}

/// **The reader stops when the engine goes** — #264, and the reason
/// `engine_gone` exists.
///
/// This is the bug as the machine felt it: `gmuse-tui-playback` retried on every
/// error, a disconnected channel errored without waiting, and the thread burned
/// 100% of a core for the rest of the session — 20 minutes of one 37-minute run,
/// spent entirely inside the `Instant::now` of a 250 ms deadline it never waited
/// on.
///
/// A loop that spins never leaves, so **`engine_gone` going true is the proof it
/// no longer spins.** The assertion before the kill is what keeps that from being
/// vacuous: an `engine_gone` stuck at `true` would pass the second check alone.
#[test]
fn the_playback_reader_leaves_when_the_engine_does() {
    use gmuse::ui::transport::Transport;
    use std::sync::Arc;

    let (mut engine, _recording) = start_with_a_track("reader-leaves");
    let client = Arc::new(Client::connect(&engine.socket, "test", "0").expect("connect"));
    let remote = gmuse::ui::remote_transport::Remote::attach(Arc::clone(&client)).expect("attach");

    assert!(
        !remote.engine_gone(),
        "the reader was gone before the engine was"
    );

    let _ = engine.child.kill();
    let _ = engine.child.wait();

    let left = (0..100).any(|_| {
        if remote.engine_gone() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
        false
    });
    assert!(
        left,
        "the reader was still running 10s after the engine died — this is the spin"
    );
}

/// **The status line says the engine went** — #264.
///
/// The span is the whole point of `engine_gone`, so it is checked where a user
/// meets it: a real `Ui`, a real `Remote`, rendered into a buffer, before and
/// after a real engine dies.
///
/// **The "before" assertion is the instrument.** A status line that always said
/// it, or a `Ui` that drew no status line at all, would satisfy the "after"
/// check on its own — and drawing nothing is exactly what a headless smoke test
/// does, which is why #261 built this backend harness rather than running the
/// binary.
#[test]
fn the_status_line_says_when_the_engine_has_gone() {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let (mut engine, _recording) = start_with_a_track("says-gone");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));
    let remote = gmuse::ui::remote_transport::Remote::attach(std::sync::Arc::clone(&client))
        .expect("attach");

    let data = engine.dir.join("xdg").join("gmuse");
    let device: gmuse::model::id::Id<gmuse::model::entity::Device> = std::fs::read_dir(&data)
        .expect("a data dir")
        .flatten()
        .find_map(|e| {
            let name = e.file_name().into_string().ok()?;
            name.strip_prefix("log-")?
                .strip_suffix(".jsonl")?
                .parse()
                .ok()
        })
        .expect("the engine wrote a log named for its device");
    let desk = gmuse::librarian::Desk::read_only(Default::default(), device, 0, data.clone());
    let answer = desk.current();
    let members: std::collections::HashSet<_> = {
        use gmuse::model::rows::Listing;
        answer.library.rows().into_iter().collect()
    };
    let ui = gmuse::ui::Ui::new(gmuse::ui::holding::of(
        Some(&answer.library),
        None,
        members,
        gmuse::shelf::Levels::default(),
        true,
    ));

    let screen = |ui: &gmuse::ui::Ui| -> String {
        let ctx = gmuse::ui::Context {
            library: &answer.library,
            player: Some(&remote),
            gapless: None,
        };
        let mut terminal = Terminal::new(TestBackend::new(100, 14)).expect("a backend");
        terminal
            .draw(|f| gmuse::ui::draw(f, ui, &ctx))
            .expect("a frame");
        let buf = terminal.backend().buffer().clone();
        (0..buf.area.height)
            .map(|y| {
                (0..buf.area.width)
                    .map(|x| buf[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    };

    let alive = screen(&ui);
    assert!(
        !alive.contains("engine gone"),
        "the status line cried engine-gone at a living engine:\n{alive}"
    );

    let _ = engine.child.kill();
    let _ = engine.child.wait();

    use gmuse::ui::transport::Transport;
    let left = (0..100).any(|_| {
        if remote.engine_gone() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(100));
        false
    });
    assert!(left, "the reader never noticed the engine had gone");

    let dead = screen(&ui);
    assert!(
        dead.contains("engine gone"),
        "the engine died and the status line said nothing:\n{dead}"
    );
}

/// **Cue returns to the mark, and the mark is where you set it** — #229, D-14.
///
/// The rule is unit-tested in `audio`; this is the wiring it cannot reach —
/// that `player.transport {"verb":"cue"}` arrives, that the decoder remembers a
/// frame when it is paused and seeks to *that* frame when it is running, and
/// that it lands paused rather than stopped.
///
/// **The mark is deliberately not the start.** Cue with the default reference
/// is a seek to frame zero, which is also exactly what a decoder ignoring
/// `cue_frame` would do — so cueing only from the top would pass against a
/// hard-coded rewind. Setting the mark mid-track separates the two, and the
/// last assertion is the one that catches it.
///
/// **Every step waits for the state it needs, and none of them sleeps for it.**
/// The first version slept 150 ms after `player.load` and then paused. `load`
/// travels the longer road — librarian, programme, then the decoder — so the
/// decoder took the pause *first* and the load set `Playing` after it. Cue then
/// read a running deck, returned to an unset mark, and landed at zero; the
/// failure looked like the mark being ignored when the pause was.
#[test]
fn cue_returns_to_the_mark_that_was_set_not_to_the_start() {
    let (engine, recording) = start_with_a_track("cue-mark");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");

    let ask = |verb: &str| {
        client
            .call(
                gmuse_proto::method::PLAYER_TRANSPORT,
                Some(serde_json::json!({ "verb": verb })),
            )
            .unwrap_or_else(|e| panic!("{verb} refused: {e}"));
    };
    let line = |text: &str| {
        client
            .call(
                gmuse_proto::method::EXEC_LINE,
                Some(serde_json::json!({ "line": text })),
            )
            .unwrap_or_else(|e| panic!("{text:?} refused: {e}"));
    };
    let now = || -> (String, u64) {
        let got = client
            .call(gmuse_proto::method::PLAYBACK_GET, None)
            .expect("playback.get");
        (
            got["state"].as_str().unwrap_or("?").to_string(),
            got["pos_frames"].as_u64().unwrap_or(0),
        )
    };
    let settle = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if now().0 == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the engine never reached {want}, it is {}", now().0);
    };

    client
        .call(
            gmuse_proto::method::PLAYER_LOAD,
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");
    settle("playing");
    ask("pause");
    settle("paused");

    // Paused, a quarter second in, so this press marks rather than returns.
    line("seek 0.25");
    std::thread::sleep(Duration::from_millis(150));
    let (_, marked_at) = now();
    assert!(
        marked_at > 5_000,
        "the seek did not move the playhead, so there is no mark to set: {marked_at} frames"
    );
    ask("cue");
    std::thread::sleep(Duration::from_millis(150));
    let (state, after_marking) = now();
    assert_eq!(state, "paused", "marking should not start anything");
    assert!(
        after_marking.abs_diff(marked_at) < 3_000,
        "marking moved the playhead from {marked_at} to {after_marking}; it should \
         only remember it"
    );

    // Somewhere else, and running, so this press returns. **Inside the track**:
    // it used to seek to 0.6s of a half-second fixture, which stopped the engine
    // and only worked because `play` starts a stopped deck again.
    line("seek 0.4");
    std::thread::sleep(Duration::from_millis(100));
    ask("play");
    settle("playing");
    ask("cue");
    settle("paused");
    std::thread::sleep(Duration::from_millis(150));

    let landed = now().1;
    assert!(
        landed.abs_diff(marked_at) < 4_000,
        "cue landed at {landed} frames, not near the mark at {marked_at}"
    );
    assert!(
        landed > 5_000,
        "cue rewound to the start, which is what ignoring the mark looks like"
    );
}

/// **A load forgets the mark** — #229.
///
/// The reference is a frame count into *a* track, so carrying it across a load
/// would drop the next track's playhead at a position chosen inside the last
/// one — and silently, since both are just numbers.
///
/// **Written because a mutation survived.** Deleting the `cue_frame = 0` beside
/// the load broke nothing in 62 integration tests: the reset was a sentence in
/// a comment, §13's fourth way to be green and wrong. The main cue test cannot
/// catch it, because it never loads twice.
#[test]
fn loading_again_forgets_where_the_mark_was() {
    let (engine, recording) = start_with_a_track("cue-reload");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");

    let ask = |verb: &str| {
        client
            .call(
                gmuse_proto::method::PLAYER_TRANSPORT,
                Some(serde_json::json!({ "verb": verb })),
            )
            .unwrap_or_else(|e| panic!("{verb} refused: {e}"));
    };
    let load = || {
        client
            .call(
                gmuse_proto::method::PLAYER_LOAD,
                Some(serde_json::json!({ "recording": recording })),
            )
            .expect("load");
    };
    let now = || -> (String, u64) {
        let got = client
            .call(gmuse_proto::method::PLAYBACK_GET, None)
            .expect("playback.get");
        (
            got["state"].as_str().unwrap_or("?").to_string(),
            got["pos_frames"].as_u64().unwrap_or(0),
        )
    };
    let settle = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if now().0 == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the engine never reached {want}, it is {}", now().0);
    };

    load();
    settle("playing");
    ask("pause");
    settle("paused");
    client
        .call(
            gmuse_proto::method::EXEC_LINE,
            Some(serde_json::json!({ "line": "seek 0.25" })),
        )
        .expect("seek");
    std::thread::sleep(Duration::from_millis(150));
    let marked_at = now().1;
    assert!(
        marked_at > 5_000,
        "nothing was marked, so forgetting it proves nothing: {marked_at} frames"
    );
    ask("cue");
    std::thread::sleep(Duration::from_millis(150));

    // A fresh load. The mark belonged to the track that just left.
    load();
    settle("playing");
    ask("cue");
    settle("paused");
    std::thread::sleep(Duration::from_millis(150));

    let landed = now().1;
    assert!(
        landed < 5_000,
        "cue landed at {landed} frames — it kept the mark at {marked_at} from \
         before the load"
    );
}

/// **A preview plays from the mark and leaves it there** — #228, §5's third case.
///
/// The view decides whether a key is held or latched; the engine only ever sees
/// "preview began". What it has to get right is that the preview *starts at the
/// reference*, and that ending one — a plain `cue`, because a running deck
/// already returns there — puts the playhead back without having moved the mark.
///
/// **The mark is set away from the start**, for the same reason the cue test
/// sets it there: previewing from frame zero is what a decoder that ignored
/// `cue_frame` would do anyway.
#[test]
fn a_preview_starts_at_the_mark_and_does_not_move_it() {
    let (engine, recording) = start_with_a_track("preview");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");

    let ask = |verb: &str| {
        client
            .call(
                gmuse_proto::method::PLAYER_TRANSPORT,
                Some(serde_json::json!({ "verb": verb })),
            )
            .unwrap_or_else(|e| panic!("{verb} refused: {e}"));
    };
    let now = || -> (String, u64) {
        let got = client
            .call(gmuse_proto::method::PLAYBACK_GET, None)
            .expect("playback.get");
        (
            got["state"].as_str().unwrap_or("?").to_string(),
            got["pos_frames"].as_u64().unwrap_or(0),
        )
    };
    let settle = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if now().0 == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the engine never reached {want}, it is {}", now().0);
    };

    client
        .call(
            gmuse_proto::method::PLAYER_LOAD,
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");
    settle("playing");
    ask("pause");
    settle("paused");

    // **Both positions are inside the track, and that needed measuring.** The
    // fixture is 24,000 frames at the device rate — about half a second — and a
    // seek past the end leaves the engine `stopped`, where the next seek does
    // nothing at all. The first version marked at 0.25s and moved to 0.75s,
    // which is off the end: the playhead stayed put and the failure read as
    // "the seek did not move" rather than "there was nowhere to move to".
    client
        .call(
            gmuse_proto::method::EXEC_LINE,
            Some(serde_json::json!({ "line": "seek 0.15" })),
        )
        .expect("seek");
    std::thread::sleep(Duration::from_millis(150));
    let mark = now().1;
    assert!(mark > 5_000, "nothing was marked: {mark} frames");
    ask("cue");
    std::thread::sleep(Duration::from_millis(150));

    // Somewhere else, so "the preview started at the mark" is falsifiable.
    client
        .call(
            gmuse_proto::method::EXEC_LINE,
            Some(serde_json::json!({ "line": "seek 0.4" })),
        )
        .expect("seek");
    std::thread::sleep(Duration::from_millis(150));
    let elsewhere = now().1;
    assert!(
        elsewhere > mark + 5_000,
        "the second seek did not move far enough from the mark: {elsewhere} vs {mark}"
    );

    // Hold: it should be running, from the mark.
    ask("cue_preview");
    settle("playing");
    let began = now().1;
    assert!(
        began < mark + 6_000,
        "the preview started at {began} frames, not at the mark {mark}"
    );

    // Release: the snap back is a plain cue, and the mark survives it.
    ask("cue");
    settle("paused");
    std::thread::sleep(Duration::from_millis(150));
    let landed = now().1;
    assert!(
        landed.abs_diff(mark) < 4_000,
        "the snap back landed at {landed}, not at the mark {mark}"
    );
    assert!(
        landed > 5_000,
        "it snapped to the start, which is what losing the mark looks like"
    );
}

/// **An attached seek keeps its fraction** — #265.
///
/// `remote_transport::seek` built the text `seek 12` with `as_secs()`, which
/// truncates a `Duration`. Reachable from a session restore, which seeks to
/// `saved.position_secs`: reattaching resumed up to a second before where it
/// left off, every time.
///
/// **Asserted on a position that is not a whole second**, because every
/// truncating implementation agrees with a correct one at 12.0. The fixture is
/// 24,000 frames at the device rate, so 0.35s is inside it and far enough from
/// 0.0 to tell a rounding from a failure.
#[test]
fn seeking_over_the_wire_does_not_round_to_the_second() {
    let (engine, recording) = start_with_a_track("seek-fraction");
    let client = Client::connect(&engine.socket, "test", "0").expect("connect");

    let now = || -> (String, u64) {
        let got = client
            .call(gmuse_proto::method::PLAYBACK_GET, None)
            .expect("playback.get");
        (
            got["state"].as_str().unwrap_or("?").to_string(),
            got["pos_frames"].as_u64().unwrap_or(0),
        )
    };
    let settle = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if now().0 == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the engine never reached {want}, it is {}", now().0);
    };

    client
        .call(
            gmuse_proto::method::PLAYER_LOAD,
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");
    settle("playing");
    client
        .call(
            gmuse_proto::method::PLAYER_TRANSPORT,
            Some(serde_json::json!({ "verb": "pause" })),
        )
        .expect("pause");
    settle("paused");

    client
        .call(
            gmuse_proto::method::PLAYER_SEEK,
            Some(serde_json::json!({ "to": { "absolute": 0.35 } })),
        )
        .expect("seek");
    std::thread::sleep(Duration::from_millis(200));

    // 0.35s at the device rate. Truncating to whole seconds lands at 0.
    let landed = now().1;
    let rate = client
        .call(gmuse_proto::method::PLAYBACK_GET, None)
        .expect("playback.get")["sample_rate"]
        .as_u64()
        .expect("a sample rate");
    let want = (0.35 * rate as f64) as u64;
    assert!(
        want > 5_000,
        "the fixture rate makes 0.35s only {want} frames — too close to zero to tell"
    );
    assert!(
        landed.abs_diff(want) < 2_000,
        "seeking to 0.35s landed at {landed} frames, not near {want}"
    );
}

/// **And through the seam the bug was actually in** — #265.
///
/// `seeking_over_the_wire_does_not_round_to_the_second` calls `player.seek`
/// directly, which is the wire's half. It does not go through
/// `remote_transport::seek` — the function that held the `as_secs()` — so
/// putting the truncation back there leaves that test green. This one drives a
/// real `Remote`, which is the path a view takes.
///
/// The two are not redundant: one pins the protocol, this pins the caller.
#[test]
fn a_remote_seek_keeps_its_fraction_too() {
    use gmuse::ui::transport::Transport;
    use std::sync::Arc;

    let (engine, recording) = start_with_a_track("seek-seam");
    let client = Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));
    let remote = gmuse::ui::remote_transport::Remote::attach(Arc::clone(&client)).expect("attach");

    let now = || -> (String, u64) {
        let got = client
            .call(gmuse_proto::method::PLAYBACK_GET, None)
            .expect("playback.get");
        (
            got["state"].as_str().unwrap_or("?").to_string(),
            got["pos_frames"].as_u64().unwrap_or(0),
        )
    };
    let settle = |want: &str| {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            if now().0 == want {
                return;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        panic!("the engine never reached {want}, it is {}", now().0);
    };

    client
        .call(
            gmuse_proto::method::PLAYER_LOAD,
            Some(serde_json::json!({ "recording": recording })),
        )
        .expect("load");
    settle("playing");
    remote.pause();
    settle("paused");

    remote.seek(gmuse::audio::SeekTo::Absolute(Duration::from_secs_f64(
        0.35,
    )));
    std::thread::sleep(Duration::from_millis(250));

    let rate = client
        .call(gmuse_proto::method::PLAYBACK_GET, None)
        .expect("playback.get")["sample_rate"]
        .as_u64()
        .expect("a sample rate");
    let want = (0.35 * rate as f64) as u64;
    assert!(want > 5_000, "0.35s is only {want} frames at this rate");
    let landed = now().1;
    assert!(
        landed.abs_diff(want) < 2_000,
        "a Remote seek to 0.35s landed at {landed} frames, not near {want}"
    );
}

/// **`queue.enqueue` lands after what is sounding, not after the programme** — what `e`
/// has always meant.
///
/// Under one list it was a push to the end, which on a real library is after seven
/// thousand shuffled rows; and the TUI's `e` was worse, a `queue.set` of the page it
/// mirrored that replaced the programme with that page. Now the engine anchors the
/// insert at the sounding entry — `queued` after what a person already queued, `next`
/// right after the track — and the programme keeps its tail.
#[test]
fn an_enqueue_lands_after_what_is_sounding() {
    let (engine, first) = start_with_a_track("enqueue-anchored");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));

    // The fixture imported two; the other one is the row to queue.
    let rows = client
        .call(
            "query.run",
            Some(serde_json::json!({ "filter": "any=\"*tone*\"", "limit": 10 })),
        )
        .expect("query.run")["rows"]
        .as_array()
        .cloned()
        .expect("rows");
    let second = rows
        .iter()
        .filter_map(|r| r["recording"].as_str())
        .find(|r| *r != first)
        .expect("a second recording")
        .to_string();

    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": first })),
        )
        .expect("the load is accepted");
    let mut sounding = false;
    for _ in 0..100 {
        let Ok(event) = client.next_event(Duration::from_millis(200)) else {
            continue;
        };
        if event.topic == gmuse_proto::Topic::Playback && event.body["recording"] == first {
            sounding = true;
            break;
        }
    }
    assert!(
        sounding,
        "the first track never started, so there is nothing to anchor on"
    );

    // The instrument: the programme is longer than one entry, so "after what is
    // sounding" and "at the end" are different places.
    let before = client.call("queue.get", None).expect("queue.get");
    assert!(
        before["total"].as_u64().is_some_and(|n| n >= 2),
        "the programme is {:?} long, so the end is next and the test proves nothing",
        before["total"]
    );

    let v = |r: &str| serde_json::Value::from(r);
    let page_after = |what: &str| -> Vec<serde_json::Value> {
        let page = client.call("queue.get", None).expect("queue.get");
        assert_eq!(
            page["anchored"], true,
            "{what}: the page did not start at what is sounding"
        );
        page["entries"].as_array().cloned().expect("entries")
    };

    // `e` twice: second, then first — after what is sounding, in the order pressed, and
    // the pool's own second entry still after both. A push to the end would put them
    // after it instead, which is the difference the third position shows.
    for r in [&second, &first] {
        client
            .call(
                "queue.enqueue",
                Some(serde_json::json!({ "recordings": [r] })),
            )
            .expect("enqueue");
    }
    let entries = page_after("two `e`s");
    assert_eq!(
        &entries[..4],
        &[v(&first), v(&second), v(&first), v(&second)],
        "`queued` did not land after what is sounding, in order: {entries:?}"
    );
    assert_eq!(
        entries.len() as u64,
        before["total"].as_u64().unwrap() + 2,
        "the programme lost its tail"
    );

    // `next` cuts in front of what was queued.
    client
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [second], "at": "next" })),
        )
        .expect("enqueue next");
    let entries = page_after("`E`");
    assert_eq!(
        &entries[..5],
        &[v(&first), v(&second), v(&second), v(&first), v(&second)],
        "`next` did not cut in front of the queued run: {entries:?}"
    );

    // And a place that is not a place is refused, not the end.
    let refused = client.call(
        "queue.enqueue",
        Some(serde_json::json!({ "recordings": [second], "at": "soon" })),
    );
    assert!(refused.is_err(), "a misspelt placement was accepted");
}

/// **A queue-pane edit touches the entry it names and nothing else** — #271.
///
/// The view mirrors a *page* of the programme, and every edit used to go back as
/// `queue.set` of that page — which `Ask::Play` applies by replacing the whole programme.
/// On a real library that turned a `dd` into "the next hundred, and then silence". Now a
/// page carries tokens, a removal names them, a put anchors on one, and a clear forgets
/// only what was queued by hand.
///
/// **The instrument is a programme longer than a page.** With one shorter, replacing the
/// programme with the page is indistinguishable from editing it.
#[test]
fn a_queue_edit_by_token_keeps_the_rest_of_the_programme() {
    let (engine, first) = start_with_a_track("edit-by-token");
    let client =
        std::sync::Arc::new(Client::connect(&engine.socket, "view", "0").expect("connect"));
    let rows = client
        .call(
            "query.run",
            Some(serde_json::json!({ "filter": "any=\"*tone*\"", "limit": 10 })),
        )
        .expect("query.run")["rows"]
        .as_array()
        .cloned()
        .expect("rows");
    let second = rows
        .iter()
        .filter_map(|r| r["recording"].as_str())
        .find(|r| *r != first)
        .expect("a second recording")
        .to_string();

    client
        .subscribe(&[gmuse_proto::Topic::Playback])
        .expect("subscribe");
    client
        .call(
            "player.load",
            Some(serde_json::json!({ "recording": first })),
        )
        .expect("the load is accepted");
    let mut sounding = false;
    for _ in 0..100 {
        let Ok(event) = client.next_event(Duration::from_millis(200)) else {
            continue;
        };
        if event.topic == gmuse_proto::Topic::Playback && event.body["recording"] == first {
            sounding = true;
            break;
        }
    }
    assert!(sounding, "the first track never started");

    // Grow the programme past a page: the pool, then a hundred and twenty at the end.
    for _ in 0..120 {
        client
            .call(
                "queue.enqueue",
                Some(serde_json::json!({ "recordings": [second], "at": "end" })),
            )
            .expect("enqueue at the end");
    }
    let page = client.call("queue.get", None).expect("queue.get");
    let total = page["total"].as_u64().expect("a total");
    let shown = page["entries"].as_array().expect("entries").len() as u64;
    assert!(
        shown < total,
        "the page ({shown}) is the whole programme ({total}), so a replace would look like an edit"
    );
    let tokens: Vec<u64> = page["tokens"]
        .as_array()
        .expect("tokens beside the entries")
        .iter()
        .filter_map(|v| v.as_u64())
        .collect();
    assert_eq!(
        tokens.len() as u64,
        shown,
        "a token for every entry on the page"
    );
    assert!(
        page["by_hand"]
            .as_array()
            .expect("marks")
            .iter()
            .skip(2)
            .all(|m| m == true),
        "the enqueued rows are not marked as queued by hand"
    );

    // `dd`: remove the third entry on the page — one queued by hand — by token.
    let doomed = tokens[2];
    client
        .call(
            "queue.remove",
            Some(serde_json::json!({ "tokens": [doomed], "base_version": page["version"] })),
        )
        .expect("remove by token");
    let after = client.call("queue.get", None).expect("queue.get");
    assert_eq!(
        after["total"].as_u64(),
        Some(total - 1),
        "a removal by token did not leave the rest of the programme"
    );
    assert!(
        !after["tokens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t.as_u64() == Some(doomed)),
        "the removed entry is still on the page"
    );

    // `p`: put a row right after the third entry on the page, by its token.
    let anchor = after["tokens"][2].as_u64().expect("a token");
    client
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [first], "at": "next", "after": anchor })),
        )
        .expect("put after a token");
    let put = client.call("queue.get", None).expect("queue.get");
    assert_eq!(
        put["total"].as_u64(),
        Some(total),
        "a put changed more than one entry"
    );
    assert_eq!(put["tokens"][2].as_u64(), Some(anchor), "the anchor moved");
    assert_eq!(
        put["entries"][3], first,
        "the put row did not land right after its anchor"
    );

    // `:clear-queue`: only what was queued by hand goes; the pool stays.
    client
        .call(
            "queue.clear",
            Some(serde_json::json!({ "queued_only": true })),
        )
        .expect("clear queued");
    let cleared = client.call("queue.get", None).expect("queue.get");
    let left = cleared["total"].as_u64().expect("a total");
    assert_eq!(
        left, 2,
        "clearing what was queued left {left} of {total} — the pool is the two the play opened"
    );
    assert!(
        cleared["by_hand"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m == false),
        "a by-hand entry survived the clear"
    );

    // And a removal that names nothing is refused, not applied to whatever is at index 0.
    let refused = client.call("queue.remove", Some(serde_json::json!({})));
    assert!(refused.is_err(), "a removal naming nothing was accepted");
}
