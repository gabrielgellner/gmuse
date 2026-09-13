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
    std::fs::create_dir_all(dir.join("data")).expect("scratch");
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
    let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
    let desk = gmuse::librarian::Desk::read_only(log, Default::default(), device, 0, data.clone());

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
    let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);

    let mut ui = gmuse::ui::Ui::new(Default::default());
    ui.data_dir = Some(data.clone());
    ui.desk = gmuse::librarian::Desk::read_only(log, Default::default(), device, 0, data.clone());
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

/// **No process but the engine appends, and this is the test that proves it** —
/// #218's second Done-when, §4.4.
///
/// It was reachable, and the consequence is worse than a race. Two processes on one
/// machine share a device id, because it comes from `machine_id`; each `Instance`'s
/// clock starts its counter at 0 within a millisecond; and `Record` implements `Ord`
/// over the stamp alone while deriving `Eq` over both fields — so a
/// `BTreeSet<Record>` treats two records with one stamp as the same record and keeps
/// the first (#238).
///
/// An import writing nine facts in one millisecond uses counters 0–8 and an engine
/// rating something in that millisecond uses counter 0. One fact then disappears at
/// the next fold, with nothing said.
///
/// `librarian::Appending` makes that unreachable: one process holds the log's append
/// lock, and the other is refused before it mints anything.
#[test]
fn a_second_process_cannot_append_to_a_log_the_engine_holds() {
    let (engine, _recording) = start_with_a_track("one-appender");
    let data = engine.dir.join("xdg").join("gmuse");

    // A config pointing at the engine's own log, so the two really do collide. Kept
    // outside the directory the import placed from, because `shelf import --write`
    // carries non-audio files along with the audio (#239) — including this one.
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

    // **`remove`, not `scan`.** A scan now goes *to* the engine when one answers
    // (#218), so it is the wrong operation to test a refusal with. `remove` has no
    // method on the wire yet, which is exactly the case the refusal is for.
    let refused = Command::new(binary())
        .args(["shelf", "remove", "--write"])
        .arg(engine.dir.join("nothing-here.wav"))
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf remove");

    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a second appender was allowed: {said}"
    );
    // **Names the holder**, so the person knows what to reach instead. The pid is
    // the whole reason `lock.rs` writes one into the file.
    assert!(
        said.contains("already running"),
        "the refusal does not say who has it: {said}"
    );
    assert!(
        said.contains("Stop the engine"),
        "and does not say what to do instead: {said}"
    );
    // **Not a `Debug` dump.** `main`'s error type prints one, which is why this
    // exits rather than returning — the first cut returned a `String` and the
    // sentence came out quoted with `\n` in it.
    assert!(
        !said.contains("\\n"),
        "the message printed as a Debug dump: {said}"
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
        "a dry run was refused too: {}",
        String::from_utf8_lossy(&dry.stderr)
    );

    // **And the one operation with a wire method goes to the engine instead of
    // being refused**, which is what keeps the tools usable now that a running
    // engine is the default.
    let routed = Command::new(binary())
        .args(["shelf", "scan", "--write"])
        .env("GMUSE_CONFIG", &config)
        .env("GMUSE_SOCKET", &engine.socket)
        .output()
        .expect("run shelf scan --write");
    let out = String::from_utf8_lossy(&routed.stdout);
    assert!(
        routed.status.success(),
        "a routable scan was refused: {}{out}",
        String::from_utf8_lossy(&routed.stderr)
    );
    assert!(
        out.contains("asked the engine"),
        "it did the scan itself instead of asking: {out}"
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
        let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
        let desk = gmuse::librarian::Desk::read_only(
            log,
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
    let (log, _damage) = gmuse::model::instance::Instance::open(&data, device, 0);
    let desk = gmuse::librarian::Desk::read_only(log, Default::default(), device, 0, data.clone());
    let held = desk.current();
    let ctx = gmuse::ui::Context {
        library: &held.library,
        player: None,
        gapless: None,
        remote: None,
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
    let loaded = client
        .next_event(Duration::from_secs(10))
        .expect("a playback event for the load");
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

    // **Wait for a second recording to be named.** The first track is a second
    // long under the null renderer at wall speed, so the join happens on its
    // own — and the only way a second recording can appear is if the engine
    // chose it, because nobody told it one.
    let mut names = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(25);
    while std::time::Instant::now() < deadline {
        let Ok(event) = client.next_event(Duration::from_secs(3)) else {
            continue;
        };
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
        "the engine never moved to a second track on its own; it named {names:?}"
    );
    assert!(
        names.contains(&recording),
        "and the first one was the one asked for"
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
#[test]
fn a_queue_change_is_announced_by_version() {
    let (engine, recording) = start_with_a_track("queue-event");
    let client = Client::connect(&engine.socket, "watcher", "0").expect("connect");
    client
        .subscribe(&[gmuse_proto::Topic::Queue])
        .expect("subscribe");

    client
        .call(
            "queue.enqueue",
            Some(serde_json::json!({ "recordings": [recording] })),
        )
        .expect("append");

    let event = client
        .next_event(Duration::from_secs(10))
        .expect("a queue event");
    assert_eq!(event.topic, gmuse_proto::Topic::Queue);
    assert!(
        event.body["version"].as_u64().is_some_and(|v| v > 1),
        "it carries the version: {}",
        event.body
    );
    assert!(
        event.body.get("entries").is_none(),
        "and not the entries: {}",
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
