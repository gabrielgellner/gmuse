//! A unix socket, so the player can be driven when it is not in front of you.
//!
//! §1 asks for a `cmus-remote` equivalent "for media keys and status bars", and
//! that phrase is the design: the socket exists for the two things a TUI cannot
//! do — be reached from a key binding that is not focused on it, and answer a
//! question asked by something else's status line.
//!
//! **This is a client, not a second player.** That distinction is what #61 was
//! about: `--repl` owned an audio device and a library of its own, so it was a
//! second gmuse wearing a CLI. A remote client owns nothing. It writes a line
//! to the running player and prints what comes back, which is why the lock
//! (§13, 2026-09-01) does not apply to it and cannot.
//!
//! ## The protocol is a line
//!
//! One request line in, reply lines out, connection closed. The request is
//! parsed by [`crate::cmd::parse::command`] — **the same parser the `:` line
//! uses**, so there is no second command language to keep in step. That was
//! the standing complaint against `--repl`, which had `load`, `status`,
//! `library` and `find` that the `:` line did not.
//!
//! ## Two kinds of request, and why they are served differently
//!
//! - **A command** (`pause`, `vol +5`) is posted to the UI thread over a
//!   channel and applied on its next tick. Nothing is returned but `ok`,
//!   because the answer to "did it work" is the next status query.
//! - **A query** (`status`) is answered *here*, from a snapshot the UI thread
//!   publishes each tick. It never asks the UI thread anything, so a status bar
//!   polling once a second never waits on a render and can never wedge one.
//!
//! ## The stale socket problem does not exist here
//!
//! `UnixListener::bind` fails if the path is already there, which normally
//! means guessing whether a leftover socket belongs to a live process. It
//! cannot here: the lock is taken **before** this binds, so holding it proves
//! no other gmuse is running, and any socket file present is therefore stale by
//! construction. It is removed without a liveness check because the check has
//! already been made, once, by the kernel.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender, unbounded};
use thiserror::Error;

use crate::audio::State;
use crate::cmd::Command;
use crate::model::rows::Drawn;

/// The socket's name inside the state directory.
pub const NAME: &str = "socket";

/// How long a client waits for the player to answer.
///
/// A status bar runs on a timer and must not accumulate blocked processes if
/// the player is wedged; failing after a second is better than hanging.
const CLIENT_TIMEOUT: Duration = Duration::from_secs(1);

/// What a status query answers with.
///
/// A rendered line rather than a struct of fields: the format is already
/// configurable through `now_playing`, and giving the socket its own shape
/// would be a second display language for the same information.
#[derive(Debug, Clone, Default)]
pub struct Status {
    pub state: Option<State>,
    /// The current track through the `now_playing` template, when there is one.
    pub rendered: Option<String>,
    /// The track itself, so a caller can ask for its own format.
    ///
    /// Cloned only when the current track *changes*, not every tick — see
    /// `serve_remote`. A status bar that wants `{title}` alone cannot be served
    /// from a pre-rendered string, and truncating one is worse than useless
    /// here: this library's `now_playing` puts the title **last**, so cutting
    /// to fit a bar throws away the only field you were looking for.
    pub track: Option<Drawn>,
    pub position: Duration,
}

impl Status {
    /// One line, in the order a status bar wants to read it.
    fn render(&self) -> String {
        let state = match self.state {
            Some(State::Playing) => "playing",
            Some(State::Paused) => "paused",
            Some(State::Stopped) | None => "stopped",
        };
        match &self.rendered {
            Some(track) => format!("{state}\t{track}\t{}", self.position.as_secs()),
            None => state.to_string(),
        }
    }
}

/// The status the UI thread last published.
pub type Published = Arc<Mutex<Status>>;

/// Why the socket could not be served, or reached.
#[derive(Debug, Error)]
pub enum RemoteError {
    #[error("no player is running (no socket at {path})")]
    NotRunning { path: PathBuf },

    #[error("could not reach the player at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A running socket server. Dropping it removes the socket file.
pub struct Server {
    /// Commands the UI thread should apply on its next tick.
    pub commands: Receiver<Command>,
    /// Where the UI thread writes what a query should see.
    pub status: Published,
    path: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best effort: a socket left behind is removed by the next launch
        // anyway, because the lock proves it is stale.
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Bind the socket and serve it from a thread of its own.
///
/// **This is the fourth thread, and §3 says three.** The amendment is recorded
/// in the Decision Log: §3's rule exists to keep the *audio* path glitch-free,
/// and this thread is not on it — it never touches the ring buffer, the
/// decoder, or the callback, and the three threads that carry audio are
/// unchanged. What it does is block on `accept`, which is precisely the work
/// that must not happen on a thread with a deadline.
pub fn serve(path: &Path) -> Result<Server, RemoteError> {
    let io = |source| RemoteError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(io)?;
    }
    // Stale by construction — see the module docs. The lock was taken first.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).map_err(io)?;

    let (tx, commands) = unbounded();
    let status: Published = Arc::new(Mutex::new(Status::default()));
    let published = Arc::clone(&status);

    std::thread::Builder::new()
        .name("gmuse-remote".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                // One at a time. Clients are a line long and a status bar polls
                // once a second; a connection pool would be machinery for a
                // load that does not exist.
                handle(stream, &tx, &published);
            }
        })
        .map_err(io)?;

    Ok(Server {
        commands,
        status,
        path: path.to_path_buf(),
    })
}

/// Serve one connection: read a line, answer it, hang up.
fn handle(stream: UnixStream, tx: &Sender<Command>, status: &Published) {
    let mut reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    let reply = answer(line.trim(), tx, status);
    let mut stream = stream;
    let _ = writeln!(stream, "{reply}");
    let _ = stream.flush();
}

/// What one request line gets back.
///
/// Split out from the socket so it can be tested without one.
pub fn answer(line: &str, tx: &Sender<Command>, status: &Published) -> String {
    if line.is_empty() {
        return "err empty request".into();
    }
    // The one verb the `:` line does not have, because in the TUI the status is
    // simply on screen. Here it is the whole reason a status bar connects.
    if let Some(rest) = line.strip_prefix("status") {
        let held = match status.lock() {
            Ok(s) => s.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        };
        let rest = rest.trim();
        // `status` alone is the human answer. `status <format>` is the one a
        // status bar wants, and it is **the same format language** the config
        // and the `:` line use — so asking for `{title}` here needs nothing
        // learned and adds no second display vocabulary.
        return if rest.is_empty() {
            held.render()
        } else {
            render_with(&held, rest)
        };
    }
    let cmd = match crate::cmd::parse::command(line) {
        Ok(cmd) => cmd,
        Err(e) => return format!("err {e}"),
    };
    if !is_remotable(&cmd) {
        // `play` earns its own sentence: refusing the most obvious transport
        // verb with "needs a cursor" would read as a bug rather than a fact
        // about what it means here.
        if matches!(cmd, Command::Play) {
            return "err play means \"play what the cursor is on\"; use `pause` to resume, \
                    or `next` to start from the pool"
                .into();
        }
        return format!("err {line:?} needs a cursor; it only means something in the TUI");
    }
    match tx.send(cmd) {
        Ok(()) => "ok".into(),
        Err(_) => "err the player is shutting down".into(),
    }
}

/// Render the current track through a caller-supplied format string.
///
/// Errors are returned rather than swallowed: a status bar with a typo in its
/// template should say so once, not print a blank field forever.
fn render_with(status: &Status, template: &str) -> String {
    let Some(track) = &status.track else {
        // Nothing playing is not an error, and a status bar wants an empty
        // string for it rather than a word it then has to filter out.
        return String::new();
    };
    match crate::query::format::Template::parse(template) {
        Ok(t) => t.render(&track.listed()),
        Err(e) => format!("err {e}"),
    }
}

/// Whether a command means anything with no screen and no cursor.
///
/// The line is almost exactly "does this need a cursor". Transport, volume,
/// rating and settings change what the *player does* and travel fine. Motion,
/// folds, yanking, views and search change what you are *looking at*, and
/// sending them to a program nobody is looking at is meaningless — so they are
/// refused by name rather than silently accepted and lost.
///
/// **`play` is not on the list, and that surprised me.** It reads like the most
/// obvious transport verb there is, but in this program it means *play what the
/// cursor is on* — `execute` starts `ui.selected_track(ctx)`. Sent down a
/// socket it would start whatever row the player happens to be sitting on,
/// which is not something the caller can know. `next` is the cursor-free way to
/// begin: `advance` resolves from the **pool** through `next()`, so it starts
/// playback from a stopped player without consulting the screen at all.
///
/// Listed by what passes rather than what fails: a new `Command` variant should
/// have to be considered, not inherit remotability by being forgotten.
pub fn is_remotable(cmd: &Command) -> bool {
    matches!(
        cmd,
        Command::Pause
            | Command::Stop
            | Command::Next
            | Command::Prev
            | Command::Seek(_)
            | Command::Vol(_)
            | Command::Rate(_)
            | Command::Rescan
            | Command::Set { .. }
            | Command::Toggle(_)
    )
}

/// Send one line to the running player and return its reply.
pub fn send(path: &Path, line: &str) -> Result<String, RemoteError> {
    if !path.exists() {
        return Err(RemoteError::NotRunning {
            path: path.to_path_buf(),
        });
    }
    let io = |source| RemoteError::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut stream = UnixStream::connect(path).map_err(io)?;
    stream.set_read_timeout(Some(CLIENT_TIMEOUT)).map_err(io)?;
    stream.set_write_timeout(Some(CLIENT_TIMEOUT)).map_err(io)?;
    writeln!(stream, "{line}").map_err(io)?;
    stream.flush().map_err(io)?;

    let mut reply = String::new();
    BufReader::new(stream).read_line(&mut reply).map_err(io)?;
    Ok(reply.trim_end().to_string())
}

/// `$XDG_STATE_HOME/gmuse/socket`, beside the lock it depends on.
pub fn default_path() -> PathBuf {
    crate::config::default_state_dir().join(NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::VolSpec;

    fn published(status: Status) -> Published {
        Arc::new(Mutex::new(status))
    }

    /// The row the remote holds, as the player hands it over: owned, because
    /// it is read from another thread.
    fn a_track() -> Drawn {
        Drawn {
            artist: Some("Boards of Canada".into()),
            album_artist: Some("Boards of Canada".into()),
            file_under: "Boards of Canada".into(),
            album: "Music Has the Right to Children".into(),
            album_if_named: Some("Music Has the Right to Children".into()),
            title: "Roygbiv".into(),
            title_if_named: Some("Roygbiv".into()),
            ..Drawn::default()
        }
    }

    #[test]
    fn play_is_refused_and_says_what_to_send_instead() {
        // Not an arbitrary exclusion: `play` starts the *selected* track, so
        // down a socket it would start whatever row the cursor sits on.
        let (tx, rx) = unbounded();
        let reply = answer("play", &tx, &published(Status::default()));
        assert!(reply.starts_with("err"), "got {reply:?}");
        assert!(
            reply.contains("next"),
            "should name the alternative: {reply:?}"
        );
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn next_travels_because_it_resolves_from_the_pool() {
        let (tx, rx) = unbounded();
        assert_eq!(answer("next", &tx, &published(Status::default())), "ok");
        assert_eq!(rx.try_recv().expect("posted"), Command::Next);
    }

    #[test]
    fn a_transport_command_is_posted_and_acknowledged() {
        let (tx, rx) = unbounded();
        assert_eq!(answer("pause", &tx, &published(Status::default())), "ok");
        assert_eq!(rx.try_recv().expect("posted"), Command::Pause);
    }

    #[test]
    fn a_navigation_command_is_refused_by_name() {
        let (tx, rx) = unbounded();
        let reply = answer("down", &tx, &published(Status::default()));
        assert!(reply.starts_with("err"), "got {reply:?}");
        assert!(reply.contains("cursor"), "should say why: {reply:?}");
        assert!(
            rx.try_recv().is_err(),
            "a refused command must not reach the UI"
        );
    }

    #[test]
    fn an_unparseable_line_reports_the_parse_error() {
        let (tx, _rx) = unbounded();
        let reply = answer("frobnicate", &tx, &published(Status::default()));
        assert!(reply.starts_with("err"), "got {reply:?}");
    }

    #[test]
    fn status_is_answered_without_troubling_the_ui_thread() {
        let (tx, rx) = unbounded();
        let status = published(Status {
            state: Some(State::Playing),
            rendered: Some("Boards of Canada - Roygbiv".into()),
            track: None,
            position: Duration::from_secs(72),
        });
        assert_eq!(
            answer("status", &tx, &status),
            "playing\tBoards of Canada - Roygbiv\t72"
        );
        assert!(
            rx.try_recv().is_err(),
            "a query must not post a command at all"
        );
    }

    #[test]
    fn status_takes_a_format_so_a_status_bar_picks_its_own_fields() {
        // The reason this exists: `now_playing` puts the title last, so a
        // status bar that truncated the rendered line would throw away the one
        // field it wanted.
        let (tx, rx) = unbounded();
        let status = published(Status {
            state: Some(State::Playing),
            rendered: Some("ignored".into()),
            track: Some(a_track()),
            position: Duration::from_secs(9),
        });
        assert_eq!(answer("status {title}", &tx, &status), "Roygbiv");
        assert_eq!(
            answer("status {title} — {artist}", &tx, &status),
            "Roygbiv — Boards of Canada"
        );
        assert!(rx.try_recv().is_err(), "a query posts no command");
    }

    #[test]
    fn a_format_with_nothing_playing_is_empty_rather_than_a_word() {
        let (tx, _rx) = unbounded();
        assert_eq!(
            answer("status {title}", &tx, &published(Status::default())),
            ""
        );
    }

    #[test]
    fn a_bad_format_says_so_once_rather_than_printing_blank_forever() {
        let (tx, _rx) = unbounded();
        let status = published(Status {
            track: Some(a_track()),
            ..Status::default()
        });
        let reply = answer("status {nosuchfield}", &tx, &status);
        assert!(reply.starts_with("err"), "got {reply:?}");
    }

    #[test]
    fn status_with_nothing_loaded_is_just_the_state() {
        let (tx, _rx) = unbounded();
        assert_eq!(
            answer("status", &tx, &published(Status::default())),
            "stopped"
        );
    }

    #[test]
    fn a_command_with_an_argument_survives_the_round_trip() {
        let (tx, rx) = unbounded();
        assert_eq!(answer("vol +5", &tx, &published(Status::default())), "ok");
        assert_eq!(
            rx.try_recv().expect("posted"),
            // `+5` is five *percent*, kept as a fraction — the same value the
            // `:` line produces, which is the point of sharing the parser.
            Command::Vol(VolSpec::Relative(0.05))
        );
    }

    #[test]
    fn an_empty_line_is_an_error_rather_than_a_silent_ok() {
        let (tx, rx) = unbounded();
        assert!(answer("", &tx, &published(Status::default())).starts_with("err"));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn the_socket_round_trips_over_a_real_connection() {
        let dir = std::env::temp_dir().join(format!("gmuse-remote-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("socket");
        let server = serve(&path).expect("bind");
        *server.status.lock().unwrap() = Status {
            state: Some(State::Paused),
            rendered: Some("A Track".into()),
            track: Some(a_track()),
            position: Duration::from_secs(5),
        };

        assert_eq!(send(&path, "status").unwrap(), "paused\tA Track\t5");
        assert_eq!(send(&path, "pause").unwrap(), "ok");
        assert_eq!(server.commands.try_recv().expect("posted"), Command::Pause);
    }

    #[test]
    fn binding_over_a_stale_socket_file_succeeds() {
        // The lock proves no other player is running, so a leftover file is
        // stale by construction and is simply replaced.
        let dir = std::env::temp_dir().join(format!("gmuse-stale-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("socket");
        std::fs::write(&path, b"not a socket").unwrap();
        serve(&path).expect("a stale file must not stop the bind");
    }

    #[test]
    fn reaching_a_socket_that_is_not_there_says_no_player_is_running() {
        let path = std::env::temp_dir().join("gmuse-definitely-absent.socket");
        let _ = std::fs::remove_file(&path);
        match send(&path, "status") {
            Err(RemoteError::NotRunning { .. }) => {}
            other => panic!("expected NotRunning, got {other:?}"),
        }
    }
}
