//! **Connect to the engine, starting one if none answers** — #219, §4.2.
//!
//! The tmux model, and the reason §4.2 describes it that way: a person types
//! `gmuse` and gets a player, without having to know a daemon exists.
//!
//! ## The client starts the server, not the other way round
//!
//! `gmuse engine` implements only `--foreground` (#214 said so out loud rather
//! than accepting a flag it ignored), and nothing here needs that to change: the
//! **client** spawns a detached child and returns. That is what tmux does, and
//! it avoids the engine having to daemonize itself — no double fork, no
//! `setsid`, no PID file, and the child's lifetime is the socket's rather than a
//! process tree's.
//!
//! ## Why the race is benign
//!
//! Two views starting at once both find nothing listening and both spawn an
//! engine. Only one can bind, because the loser cannot take `engine.lock` — and
//! #214 made that refusal a clean `EngineError::Running` rather than a crash or
//! a half-bound socket. So the loser exits, the winner listens, and both views
//! connect to it. Nothing here has to coordinate.

use std::path::Path;
use std::time::{Duration, Instant};

use gmuse_client::{Client, ClientError};

/// How long to wait for a freshly spawned engine to bind.
///
/// **Generous, because the engine folds the log before it listens.** `Desk::open`
/// folds once synchronously so no client ever sees an empty library, and on a
/// real library that is hundreds of milliseconds. A timeout tuned to an empty
/// one would fail exactly on the machines that have music on them.
const START_WITHIN: Duration = Duration::from_secs(20);

/// Connect, starting an engine if nothing answers.
pub fn attach(name: &str, version: &str) -> Result<Client, ClientError> {
    let path = super::socket::default_path();
    if let Ok(client) = Client::connect(&path, name, version) {
        return Ok(client);
    }

    spawn_engine();

    let deadline = Instant::now() + START_WITHIN;
    let mut last = None;
    while Instant::now() < deadline {
        match Client::connect(&path, name, version) {
            Ok(client) => return Ok(client),
            Err(e) => last = Some(e),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(last.unwrap_or(ClientError::TimedOut(START_WITHIN)))
}

/// Start a detached engine and do not wait for it.
fn spawn_engine() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    // **stdio to null, deliberately.** The engine writes to stderr, and a child
    // sharing the terminal would draw over the TUI's alternate screen — the same
    // failure `remote.rs` documents for its error callback. Its own log is where
    // that belongs.
    let _ = std::process::Command::new(exe)
        .args(["engine", "--foreground"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
}

/// Whether an engine is listening, without starting one.
pub fn listening() -> bool {
    let path = super::socket::default_path();
    path.exists() && std::os::unix::net::UnixStream::connect(&path).is_ok()
}

/// Where it would be.
pub fn socket() -> std::path::PathBuf {
    super::socket::default_path()
}

/// Whether that path is a live socket rather than a leftover.
pub fn is_live(path: &Path) -> bool {
    std::os::unix::net::UnixStream::connect(path).is_ok()
}
