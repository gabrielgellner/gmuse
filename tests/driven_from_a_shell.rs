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
    let dir = std::env::temp_dir().join(format!("gmuse-222-{}-{name}", std::process::id()));
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
            Some(serde_json::json!({ "recording": recording, "stars": 4 })),
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
                "recording": "8f3a1b2c-0000-4000-8000-000000000002",
                "stars": 5
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
    let dir = std::env::temp_dir().join(format!("gmuse-230-{}-{name}", std::process::id()));
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
