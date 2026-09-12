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

    // A command that parses gets as far as the engine can take it today.
    let (_out, err, ok) = ctl(&engine.socket, &["seek", "+10"]);
    assert!(!ok);
    assert!(
        err.contains("1007"),
        "parsed, and nothing to apply it to: {err}"
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
    assert_eq!(value["code"], 1007);
    assert_eq!(value["data"]["parsed"], "seek +10");
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
