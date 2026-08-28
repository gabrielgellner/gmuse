//! M2 — the audio API.
//!
//! `PlayerHandle` (in `audio`) is the whole interface: load, play, pause,
//! stop, seek, volume, position, and an event stream. This binary drives it
//! from stdin lines, standing in for the UI until M4.
//!
//! Verification target from DESIGN §9: seek is sample-accurate and the
//! position display does not jump on seek.

mod audio;
mod cli;
mod repl;

use std::io::{BufRead, Write};
use std::time::Duration;

use clap::Parser;

use audio::{PlayerEvent, PlayerHandle, State};
use cli::Cli;
use repl::Line;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Cli::parse();
    let player = PlayerHandle::new(args.player_config())?;
    println!("gmuse — device at {} Hz", player.device_rate());

    match &args.file {
        Some(path) => player.load(path.clone()),
        None => println!("{}", repl::HELP),
    }

    let stdin = std::io::stdin();
    let mut lines = stdin.lock().lines();

    loop {
        drain_events(&player);

        print!("> ");
        std::io::stdout().flush()?;

        let Some(line) = lines.next() else { break };
        let line = line?;

        match repl::parse(&line) {
            Ok(Line::Quit) => break,
            Ok(Line::Empty) => {}
            Ok(cmd) => run(&player, cmd),
            Err(e) => println!("{e}"),
        }

        // Give the decoder a moment so its reply lands in this iteration
        // rather than looking like it belongs to the next command.
        std::thread::sleep(Duration::from_millis(60));
        drain_events(&player);
    }

    Ok(())
}

fn run(player: &PlayerHandle, cmd: Line) {
    match cmd {
        Line::Load(p) => player.load(p),
        Line::Play => player.play(),
        Line::Pause => player.pause(),
        Line::Toggle => player.toggle(),
        Line::Stop => player.stop(),
        Line::Seek(to) => player.seek(to),
        Line::Volume(v) => {
            player.set_volume(v);
            println!("volume {:.2}", player.volume());
        }
        Line::Status => print_status(player),
        Line::Help => println!("{}", repl::HELP),
        Line::Quit | Line::Empty => {}
    }
}

fn print_status(player: &PlayerHandle) {
    let state = match player.state() {
        State::Playing => "playing",
        State::Paused => "paused",
        State::Stopped => "stopped",
    };
    println!(
        "{state}  {}  vol {:.2}  underruns {}",
        fmt_time(player.position()),
        player.volume(),
        player.underruns()
    );
}

fn drain_events(player: &PlayerHandle) {
    while let Ok(event) = player.events().try_recv() {
        match event {
            PlayerEvent::Loaded {
                path,
                duration,
                source_rate,
                channels,
                resampling,
            } => {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.display().to_string());
                let len = duration.map(fmt_time).unwrap_or_else(|| "??:??".into());
                let conv = if resampling { " (resampling)" } else { "" };
                println!("\n♪ {name}  [{len}]  {source_rate} Hz, {channels} ch{conv}");
            }
            PlayerEvent::Finished => println!("\nfinished"),
            PlayerEvent::Error(e) => println!("\nerror: {e}"),
            PlayerEvent::DeviceLost(e) => println!("\ndevice lost: {e}"),
        }
    }
}

/// `mm:ss.mmm` — millisecond resolution, because second resolution cannot show
/// whether a seek landed where it was asked to.
fn fmt_time(d: Duration) -> String {
    let total = d.as_secs_f64();
    let mins = (total / 60.0).floor();
    format!("{:02}:{:06.3}", mins as u64, total - mins * 60.0)
}
