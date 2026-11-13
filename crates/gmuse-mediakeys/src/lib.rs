//! **The keyboard's media keys, as one more client of the engine** —
//! `gmuse-engine-architecture.md` §13.
//!
//! A process that sits between the operating system and the engine's socket. A
//! key press becomes the request `gmuse ctl` would have sent; a `playback` event
//! becomes what the system shows as now playing. The engine learns nothing about
//! either: it sees a client called `gmuse-mediakeys`.
//!
//! ## Why it is a process and not a thread in the engine
//!
//! macOS delivers a key to a handler on the **main thread's run loop**, and only
//! to a process that has said what it is playing. The engine's main thread waits
//! on a flag and its audio path is real-time; a run loop and an Objective-C
//! runtime are not things to add to either. As a client, this can crash without a
//! dropout, and the engine can be built and tested without it.
//!
//! ## The two halves
//!
//! Everything here is the half that is not the operating system: which request a
//! key means ([`Key::request`]), and what the engine's replies say is playing
//! ([`NowPlaying`]). It is plain data, so it is tested without a keyboard. The
//! other half is `macos.rs`, which is the only file that names the platform.

use std::path::Path;
use std::time::Duration;

use gmuse_proto::request::{Seek, Transport, method};
use serde::Deserialize;

#[cfg(target_os = "macos")]
mod macos;

/// The name this client gives in its `hello`.
pub const CLIENT_NAME: &str = "gmuse-mediakeys";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Client(#[from] gmuse_client::ClientError),
    #[error("the system's media controls refused: {0}")]
    Controls(String),
    #[error("could not start the thread that follows the engine: {0}")]
    Thread(#[from] std::io::Error),
    /// Said out loud, the way `gmuse engine` says it only runs in the foreground:
    /// a command that started and did nothing would look like one that worked.
    #[error("media keys are built for macOS only, so far")]
    Unsupported,
}

/// Follow the engine at `socket` and answer the media keys until it goes away.
///
/// **Must be called on the main thread**, which is where macOS delivers a key.
/// Returns only to say why it could not start; once running it ends by exiting
/// the process when the engine closes the connection.
///
/// `here` is this machine's device, as the library names it. It is what lets the
/// engine say which file it would play, and so which folder the cover is in; the
/// caller derives it, because this crate cannot name the library that defines it.
pub fn run(socket: &Path, version: &str, here: &str) -> Result<(), Error> {
    #[cfg(target_os = "macos")]
    {
        macos::run(socket, version, here)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (socket, version, here);
        Err(Error::Unsupported)
    }
}

/// One thing a media key can ask for.
///
/// **Our own type rather than the platform's**, so the mapping onto the wire is
/// written once and tested on any machine: `macos.rs` turns its events into
/// these, and MPRIS would do the same.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Key {
    Play,
    Pause,
    /// **Sent as `toggle`, never resolved here** — `transport-keymap-spec.md`
    /// D-8. This process knows what was playing as of the last event, which is
    /// exactly the stale read that makes two quick presses both send `play`.
    Toggle,
    Next,
    Previous,
    /// The scrubber in Control Centre, dragged to a place.
    SeekTo(Duration),
    /// Seconds on from here, or back when negative.
    SeekBy(f64),
}

impl Key {
    /// The method and params that ask the engine for it.
    ///
    /// **`next` and `prev` go through the `:` line**, because no typed method
    /// moves through the programme yet: `playback-programme.md` gives that to the
    /// librarian and it is not built. The line is the same one `gmuse ctl next`
    /// sends, so this adds no second way to say it.
    pub fn request(self) -> (&'static str, serde_json::Value) {
        let transport = |verb: Transport| {
            (
                method::PLAYER_TRANSPORT,
                serde_json::json!({ "verb": verb }),
            )
        };
        let line = |line: &str| (method::EXEC_LINE, serde_json::json!({ "line": line }));
        let seek = |to: Seek| (method::PLAYER_SEEK, serde_json::json!({ "to": to }));
        match self {
            Key::Play => transport(Transport::Play),
            Key::Pause => transport(Transport::Pause),
            Key::Toggle => transport(Transport::Toggle),
            Key::Next => line("next"),
            Key::Previous => line("prev"),
            Key::SeekTo(at) => seek(Seek::Absolute(at.as_secs_f64())),
            Key::SeekBy(seconds) => seek(Seek::Relative(seconds)),
        }
    }
}

/// Whether the stream is running, as `playback.get` spells it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Playing,
    Paused,
    Stopped,
}

/// The format `playback.get` is asked to render, and [`NowPlaying::read`] splits.
///
/// **Separated by U+001F, the unit separator**, which is what the character is
/// for and which no title contains. Each field sits in a group, so a missing one
/// renders as nothing between its separators rather than shifting the rest.
pub const FORMAT: &str = "[{title}]\u{1f}[{artist}]\u{1f}[{album}]";

/// What the system is told is playing.
#[derive(Debug, Clone, PartialEq)]
pub struct NowPlaying {
    pub state: State,
    /// Which recording, so a caller can tell a new track from a seek within one.
    pub recording: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    /// Where the playhead was when the engine answered. The system extrapolates
    /// from here, which is §7.8's rule for every client: no position stream.
    pub position: Duration,
    /// How long the track is. Not in `playback.get`'s reply — see [`length_of`].
    pub length: Option<Duration>,
    /// The picture beside the file that is playing — see [`cover_among`].
    pub cover: Option<std::path::PathBuf>,
}

impl NowPlaying {
    /// Read a `playback.get` reply that was asked for [`FORMAT`].
    pub fn read(reply: &serde_json::Value) -> Result<Self, serde_json::Error> {
        #[derive(Deserialize)]
        struct Reply {
            state: State,
            #[serde(default)]
            recording: Option<String>,
            // **`Option`, because the engine says `null` and not nothing.** An
            // engine with no stream answers some of these as an explicit null, and
            // `#[serde(default)]` only covers a field that is absent.
            #[serde(default)]
            pos_frames: Option<u64>,
            #[serde(default)]
            sample_rate: Option<u32>,
            #[serde(default)]
            rendered: Option<String>,
        }
        let reply = Reply::deserialize(reply)?;

        let rendered = reply.rendered.unwrap_or_default();
        let mut names = rendered.split('\u{1f}').map(|name| {
            Some(name.trim())
                .filter(|n| !n.is_empty())
                .map(str::to_owned)
        });
        let mut name = || names.next().flatten();

        Ok(Self {
            state: reply.state,
            recording: reply.recording,
            title: name(),
            artist: name(),
            album: name(),
            // A rate of zero is an engine with no stream open, and there is no
            // position to speak of rather than an infinite one.
            position: match (reply.pos_frames, reply.sample_rate) {
                (Some(frames), Some(rate)) if rate > 0 => {
                    Duration::from_secs_f64(frames as f64 / f64::from(rate))
                }
                _ => Duration::ZERO,
            },
            length: None,
            cover: None,
        })
    }
}

/// How long a recording is, from a `query.recording` reply.
///
/// **A second question because the first cannot answer it as a number.** The
/// format language renders a duration as `6:32`, for a person; reading that back
/// would be parsing our own presentation. The row carries the length typed.
pub fn length_of(reply: &serde_json::Value) -> Option<Duration> {
    #[derive(Deserialize)]
    struct Reply {
        row: Row,
    }
    #[derive(Deserialize)]
    struct Row {
        length: Duration,
    }
    Reply::deserialize(reply).ok().map(|reply| reply.row.length)
}

/// Which file the engine would play, from a `query.recording` reply asked with this
/// device as `here`.
///
/// `chosen` is `[locator, encoding, levels]`, and `null` when this device holds no
/// copy it can reach — a drive not mounted, or a recording held only elsewhere.
pub fn locator_of(reply: &serde_json::Value) -> Option<std::path::PathBuf> {
    reply["chosen"][0].as_str().map(std::path::PathBuf::from)
}

/// **The cover, by name, from what is in the album's folder.**
///
/// `cover` before `folder` before `front`, each as `jpg`, `jpeg` or `png`, in any
/// case. Measured on the live library on 2026-11-13: of 1,201 album folders, 1,162
/// hold a `cover.jpg` and 15 a `cover.png`, and `covers fetch-apply` writes
/// `cover.jpg` (`renderings.md` §5), so the first name is the library's own
/// convention and the other two are what other software left before it.
///
/// **A name and not a guess.** `cover-small.jpg` is the copy an upgrade kept, and
/// `side a.jpg`, `cd.jpeg` and `back.bmp` are pictures of something else; any image
/// in the folder would show the back of a record whenever its name sorted first.
/// A folder with none of these shows no picture, which is honest.
pub fn cover_among<'a>(names: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    const STEMS: [&str; 3] = ["cover", "folder", "front"];
    const EXTENSIONS: [&str; 3] = ["jpg", "jpeg", "png"];
    let rank = |name: &str| {
        let lower = name.to_ascii_lowercase();
        let (stem, extension) = lower.rsplit_once('.')?;
        let stem = STEMS.iter().position(|s| *s == stem)?;
        let extension = EXTENSIONS.iter().position(|e| *e == extension)?;
        Some((stem, extension))
    };
    names
        .into_iter()
        .filter_map(|name| Some((rank(name)?, name)))
        .min()
        .map(|(_, name)| name)
}

/// A path as a `file://` URL, which is what the system's artwork loader reads.
///
/// **Percent-encoded byte by byte**, because a URL is parsed and a path is not: a
/// space, a `#` or a `?` in an album name would otherwise end the path or start a
/// fragment, and `NSURL` answers a string it cannot parse with nothing at all.
pub fn file_url(path: &Path) -> String {
    let mut url = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(char::from(byte));
            }
            other => url.push_str(&format!("%{other:02X}")),
        }
    }
    url
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmuse_proto::request::Request;

    /// **Every key is a request the engine would accept, and the one intended.**
    ///
    /// Asked of `Request::of`, the parser the engine runs on what arrives, rather
    /// than compared to JSON written out a second time here: a params object with
    /// a misspelt field would match its own copy and be refused on the wire.
    #[test]
    fn every_key_is_the_request_the_engine_would_read_it_as() {
        let read = |key: Key| {
            let (method, params) = key.request();
            Request::of(method, Some(params)).unwrap_or_else(|e| panic!("{key:?}: {e:?}"))
        };
        let transport = |verb| Request::PlayerTransport { verb };

        assert_eq!(read(Key::Play), transport(Transport::Play));
        assert_eq!(read(Key::Pause), transport(Transport::Pause));
        assert_eq!(read(Key::Toggle), transport(Transport::Toggle));
        assert_eq!(
            read(Key::SeekTo(Duration::from_millis(90_500))),
            Request::PlayerSeek {
                to: Seek::Absolute(90.5)
            }
        );
        assert_eq!(
            read(Key::SeekBy(-10.0)),
            Request::PlayerSeek {
                to: Seek::Relative(-10.0)
            }
        );
        assert_eq!(
            read(Key::Next),
            Request::ExecLine {
                line: "next".into()
            }
        );
        assert_eq!(
            read(Key::Previous),
            Request::ExecLine {
                line: "prev".into()
            }
        );
    }

    /// The shape a running engine answered with on 2026-11-12, names changed.
    fn reply(rendered: &str) -> serde_json::Value {
        serde_json::json!({
            "entry": 4738,
            "fader": 1.0,
            "pos_frames": 441_000,
            "rate": 1.0,
            "recording": "659cb1ec-4f69-5cd2-8250-670c9f920158",
            "rendered": rendered,
            "sample_rate": 44_100,
            "state": "playing",
            "stream_generation": 1,
        })
    }

    #[test]
    fn a_reply_is_read_into_what_is_playing_and_where() {
        let now = NowPlaying::read(&reply("A Title\u{1f}An Artist\u{1f}An Album")).unwrap();
        assert_eq!(now.state, State::Playing);
        assert_eq!(now.title.as_deref(), Some("A Title"));
        assert_eq!(now.artist.as_deref(), Some("An Artist"));
        assert_eq!(now.album.as_deref(), Some("An Album"));
        assert_eq!(now.position, Duration::from_secs(10));
    }

    /// **A missing field leaves its place, so the ones after it keep theirs.**
    ///
    /// A single has no album and some files have no artist. Splitting on a
    /// separator that vanished with its field would file the album as the artist.
    #[test]
    fn a_missing_name_does_not_shift_the_others() {
        let now = NowPlaying::read(&reply("A Title\u{1f}\u{1f}An Album")).unwrap();
        assert_eq!(now.title.as_deref(), Some("A Title"));
        assert_eq!(now.artist, None);
        assert_eq!(now.album.as_deref(), Some("An Album"));
    }

    /// **An idle engine is read, not refused.**
    ///
    /// The first is what an engine with nothing loaded answered on 2026-11-12. The
    /// second is the same with its numbers null, which a starting engine sent the
    /// adapter twice on the same day — and which the first version of this could not
    /// read, because its test had guessed that a missing value would be absent.
    #[test]
    fn nothing_loaded_reads_as_stopped_at_zero() {
        let idle = serde_json::json!({
            "entry": null, "fader": 1.0, "pos_frames": 0, "rate": 1.0, "recording": null,
            "rendered": "", "sample_rate": 44_100, "state": "stopped", "stream_generation": 1,
        });
        let mut nulls = idle.clone();
        nulls["pos_frames"] = serde_json::Value::Null;
        nulls["sample_rate"] = serde_json::Value::Null;
        nulls["rendered"] = serde_json::Value::Null;

        for reply in [idle, nulls] {
            let now = NowPlaying::read(&reply).unwrap_or_else(|e| panic!("{reply}: {e}"));
            assert_eq!(now.state, State::Stopped);
            assert_eq!(now.recording, None);
            assert_eq!(now.title, None);
            assert_eq!(now.position, Duration::ZERO);
        }
    }

    /// **The library's own name wins, and nothing that is merely an image is taken.**
    ///
    /// The list is the shapes the live library holds, so a rule that took the first
    /// image would be caught on the back of a record rather than on a made-up name.
    #[test]
    fn the_cover_is_chosen_by_name_and_never_by_being_a_picture() {
        let folder = [
            "01 A Track.mp3",
            "back.bmp",
            "cd.jpeg",
            "cover-small.jpg",
            "Folder.JPG",
            "side a.jpg",
            "cover.jpg",
        ];
        assert_eq!(cover_among(folder), Some("cover.jpg"));

        // Without it, the next name down, whatever its case.
        assert_eq!(
            cover_among(folder.iter().copied().filter(|n| *n != "cover.jpg")),
            Some("Folder.JPG")
        );
        // A `jpg` before a `png` of the same name.
        assert_eq!(cover_among(["cover.png", "cover.jpg"]), Some("cover.jpg"));
        // And only pictures of something else: none.
        assert_eq!(
            cover_among(["back.bmp", "cd.jpeg", "cover-small.jpg", "side a.jpg"]),
            None
        );
    }

    /// The path a real album has, which is what broke `NSURL` without the encoding.
    #[test]
    fn a_path_becomes_a_url_the_system_can_parse() {
        assert_eq!(
            file_url(Path::new(
                "/Users/me/MusicLibrary/Kimbra/Primal Heart/cover.jpg"
            )),
            "file:///Users/me/MusicLibrary/Kimbra/Primal%20Heart/cover.jpg"
        );
        assert_eq!(
            file_url(Path::new("/m/Sigur Rós/( )/#1?.jpg")),
            "file:///m/Sigur%20R%C3%B3s/%28%20%29/%231%3F.jpg"
        );
    }

    #[test]
    fn the_file_is_read_from_what_the_engine_chose() {
        let reply = serde_json::json!({
            "chosen": ["/m/Kimbra/Primal Heart/04 Like They Do on the TV.mp3", "72bf8312-70dc-5849-9f6c-17a90788369f", {}],
            "row": { "length": { "secs": 259, "nanos": 0 } },
        });
        assert_eq!(
            locator_of(&reply),
            Some(std::path::PathBuf::from(
                "/m/Kimbra/Primal Heart/04 Like They Do on the TV.mp3"
            ))
        );
        // Held nowhere this device can reach: no file, and so no cover.
        assert_eq!(
            locator_of(&serde_json::json!({ "chosen": null, "row": {} })),
            None
        );
    }

    #[test]
    fn the_length_is_read_from_the_row_as_a_number() {
        let reply = serde_json::json!({
            "chosen": null,
            "row": { "title": "A Title", "length": { "secs": 392, "nanos": 71_000_000 } },
        });
        assert_eq!(length_of(&reply), Some(Duration::new(392, 71_000_000)));
        assert_eq!(length_of(&serde_json::json!({ "row": null })), None);
    }
}
