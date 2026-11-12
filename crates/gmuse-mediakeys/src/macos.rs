//! **The macOS half**: `MPRemoteCommandCenter` in, `MPNowPlayingInfoCenter` out.
//!
//! Both through `souvlaki`, which is where the Objective-C calls and their
//! `unsafe` live. Two things about the platform shape this file.
//!
//! **A key arrives on the main thread's run loop**, so the main thread does
//! nothing but run one, and a second thread talks to the engine.
//!
//! **Only a process that says what it is playing is sent keys.** With no
//! playback state published, the play key opens Music.app instead. So the state
//! is pushed on every `playback` event, and the keys follow whichever
//! application claimed them last — a video in a browser takes them until gmuse
//! next plays, and that is the system's rule, not something to fight here.

use std::path::Path;

use crossbeam_channel::{Receiver, select, unbounded};
use gmuse_client::{Client, ClientError};
use gmuse_proto::Topic;
use gmuse_proto::request::method;
use objc2_core_foundation::CFRunLoop;
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};

use crate::{CLIENT_NAME, Error, FORMAT, Key, NowPlaying, State, length_of};

pub(crate) fn run(socket: &Path, version: &str) -> Result<(), Error> {
    let client = Client::connect(socket, CLIENT_NAME, version)?;
    // `engine` as well as `playback`: a resync says events were dropped, and the
    // answer to that is the same re-read a `playback` event gets.
    client.subscribe(&[Topic::Playback, Topic::Engine])?;

    let mut controls = MediaControls::new(PlatformConfig {
        display_name: "gmuse",
        dbus_name: "gmuse",
        hwnd: None,
    })
    .map_err(|e| Error::Controls(e.to_string()))?;

    // **The handler only forwards.** It runs on the main thread, and a round trip
    // to the engine there would hold up the next key behind the last one.
    let (pressed, keys) = unbounded();
    controls
        .attach(move |event| {
            if let Some(key) = key_of(event) {
                let _ = pressed.send(key);
            }
        })
        .map_err(|e| Error::Controls(e.to_string()))?;

    std::thread::Builder::new()
        .name("gmuse-mediakeys".into())
        .spawn(move || follow(&client, controls, &keys))?;

    // `CFRunLoopRun` returns when the loop has nothing attached. The main
    // thread's always has the main dispatch queue, so it should not — and if it
    // ever does, running it again costs nothing and returning would end the
    // process with the keys still claimed.
    loop {
        CFRunLoop::run();
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// What a system event asks for, or `None` for the ones gmuse has no answer to.
///
/// `souvlaki` registers every command the platform has; volume, shuffle, raise
/// and quit are not this player's to take from a keyboard.
fn key_of(event: MediaControlEvent) -> Option<Key> {
    let signed = |direction, seconds: f64| match direction {
        SeekDirection::Forward => seconds,
        SeekDirection::Backward => -seconds,
    };
    match event {
        MediaControlEvent::Play => Some(Key::Play),
        // **Stop is pause.** `transport-keymap-spec.md` D-9: a deck has two
        // running states, and "stopped" is paused somewhere.
        MediaControlEvent::Pause | MediaControlEvent::Stop => Some(Key::Pause),
        MediaControlEvent::Toggle => Some(Key::Toggle),
        MediaControlEvent::Next => Some(Key::Next),
        MediaControlEvent::Previous => Some(Key::Previous),
        MediaControlEvent::SetPosition(MediaPosition(at)) => Some(Key::SeekTo(at)),
        MediaControlEvent::SeekBy(direction, by) => {
            Some(Key::SeekBy(signed(direction, by.as_secs_f64())))
        }
        _ => None,
    }
}

/// Send keys to the engine and the engine's state to the system, until the engine goes.
///
/// **Every event is answered by asking again**, rather than by reading the event.
/// `playback` reports discontinuities only (§7.8), so this runs a few times a
/// track at most, and `playback.get` is the one reply that has the names in it.
fn follow(client: &Client, mut controls: MediaControls, keys: &Receiver<Key>) -> ! {
    let mut shown: Option<NowPlaying> = None;
    loop {
        // Before waiting, so the system knows what is playing from the start: a
        // key pressed before the first event still has somewhere to go.
        match now_playing(client, shown.as_ref()) {
            Ok(now) => {
                show(&mut controls, &now);
                shown = Some(now);
            }
            Err(e) => gone_or(&e, "could not ask what is playing"),
        }

        select! {
            recv(keys) -> key => {
                let Ok(key) = key else { continue };
                let (method, params) = key.request();
                if let Err(e) = client.call(method, Some(params)) {
                    gone_or(&e, "the engine refused a key");
                }
            }
            recv(client.events()) -> event => {
                if event.is_err() {
                    // The reader thread dropped the sender: the engine closed
                    // the connection, and there is nothing left to control.
                    std::process::exit(0);
                }
            }
        }
    }
}

/// What the engine says is playing, with the length carried over or looked up.
fn now_playing(client: &Client, shown: Option<&NowPlaying>) -> Result<NowPlaying, ClientError> {
    let reply = client.call(
        method::PLAYBACK_GET,
        Some(serde_json::json!({ "format": FORMAT })),
    )?;
    let mut now = NowPlaying::read(&reply)?;

    now.length = match (&now.recording, shown) {
        // The same track as before: its length has not changed.
        (Some(recording), Some(shown)) if shown.recording.as_ref() == Some(recording) => {
            shown.length
        }
        (Some(recording), _) => {
            let row = client.call(
                method::QUERY_RECORDING,
                Some(serde_json::json!({ "recording": recording })),
            )?;
            length_of(&row)
        }
        (None, _) => None,
    };
    Ok(now)
}

fn show(controls: &mut MediaControls, now: &NowPlaying) {
    // **Names first, then the state.** Setting the metadata replaces the whole
    // now-playing record, elapsed time included; the playback call writes the
    // position back into it.
    let _ = controls.set_metadata(MediaMetadata {
        title: now.title.as_deref(),
        artist: now.artist.as_deref(),
        album: now.album.as_deref(),
        duration: now.length,
        cover_url: None,
    });
    let progress = Some(MediaPosition(now.position));
    let _ = controls.set_playback(match now.state {
        State::Playing => MediaPlayback::Playing { progress },
        State::Paused => MediaPlayback::Paused { progress },
        State::Stopped => MediaPlayback::Stopped,
    });
}

/// Exit if the engine has gone; otherwise say what went wrong and carry on.
///
/// A refusal is worth a line — the engine's stderr is this process's — but not
/// worth dying for: the next key may be one it accepts.
fn gone_or(error: &ClientError, doing: &str) {
    // `Io` is the write half finding out first: a broken pipe on the request,
    // before the reader has seen the end of the stream.
    if matches!(error, ClientError::Closed | ClientError::Io(_)) {
        std::process::exit(0);
    }
    eprintln!("gmuse media-keys: {doing}: {error}");
}
