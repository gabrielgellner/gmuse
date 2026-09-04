//! Command-line interface.
//!
//! DESIGN §12: parse at the boundary. Every knob is turned into a typed value
//! here and passed down as `PlayerConfig`, so no module below reads the
//! environment or handles a raw string.

use std::path::PathBuf;

use clap::Parser;

use crate::audio::{PlayerConfig, resample::Quality};

/// gmuse — Gabriel's MUSical Experience.
///
/// A cmus-shaped terminal music player.
#[derive(Debug, Parser)]
#[command(name = "gmuse", version, about, long_about = None)]
pub struct Cli {
    /// A library operation, instead of launching the player.
    ///
    /// **A subcommand rather than another flag**, and the difference is
    /// lifetime. `--rescan` and `--migrate-log` modify a launch; these do not
    /// launch anything. `library-api.md` §2 names five of them, and they share
    /// a shape — read a file or a spec, say what would happen, and do it only
    /// when told.
    #[command(subcommand)]
    pub command: Option<Command>,

    /// Output device, matched by substring against the device id.
    #[arg(short, long, env = "GMUSE_DEVICE")]
    pub device: Option<String>,

    /// Force the device sample rate, in Hz. Mainly for measuring the cost of
    /// sample-rate conversion; normally the device picks.
    #[arg(short, long, env = "GMUSE_RATE")]
    pub rate: Option<u32>,

    /// Resampler quality.
    #[arg(short = 'q', long, env = "GMUSE_RESAMPLE", default_value = "balanced")]
    pub resample: Quality,

    /// Start fresh: do not restore the last session, and do not save this one.
    #[arg(long)]
    pub no_resume: bool,

    /// Initial volume as a fader position: 1.0 is unity, 0.0 silence.
    #[arg(short = 'v', long, default_value_t = 1.0)]
    pub volume: f32,

    /// Library root to scan. Repeat for several. Remembered in the cache, so
    /// later runs need no argument.
    #[arg(
        short = 'L',
        long = "library",
        env = "GMUSE_LIBRARY",
        value_delimiter = ':'
    )]
    pub library: Vec<PathBuf>,

    /// Move ratings, playlists and event logs from the previous `data_dir`.
    ///
    /// Not automatic: gmuse cannot tell a moved `data_dir` from a first run,
    /// or from another machine's data not having synced yet, and copying from
    /// somewhere else on a guess is how you attach one library's ratings to
    /// another (#27).
    #[arg(long)]
    pub migrate_data: bool,

    /// Convert the path-keyed event log into the identity-keyed one, once.
    ///
    /// Scans the library so it can answer what each recorded path *was*, then
    /// writes the facts it could place and names the ones it could not. It
    /// reads the old log and never deletes it, so it can be run again after a
    /// scan that identifies more (#82).
    #[arg(long)]
    pub migrate_log: bool,

    /// Re-read every file, ignoring the mtime fast path.
    #[arg(long)]
    pub rescan: bool,

    /// Do not read or write the library cache.
    #[arg(long)]
    pub no_cache: bool,

    /// Where the library cache lives.
    #[arg(long, env = "GMUSE_CACHE")]
    pub cache: Option<PathBuf>,

    /// TOML config file. Defaults to $XDG_CONFIG_HOME/gmuse/config.toml.
    #[arg(long, env = "GMUSE_CONFIG")]
    pub config: Option<PathBuf>,

    /// Skip the config file entirely.
    #[arg(long)]
    pub no_config: bool,

    /// Log the session for auditing playback. With no path, writes to
    /// $XDG_STATE_HOME/gmuse/session.log.
    ///
    /// `Option<Option<_>>` is clap's idiom for a flag whose value is optional:
    /// absent is `None`, bare `--log` is `Some(None)`, and `--log <path>` is
    /// `Some(Some(path))`. An empty `default_missing_value` does not work —
    /// clap ignores it and then insists on a value.
    #[arg(long, num_args = 0..=1, env = "GMUSE_LOG")]
    pub log: Option<Option<PathBuf>>,

    /// Sort artists ignoring a leading "The"/"A"/"An".
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    pub smart_artist_sort: bool,

    /// Send one command to the running player and print its reply, then exit.
    ///
    /// A client, not a player: it opens no audio device, reads no library and
    /// takes no lock. `--remote status` is what a status bar asks; the rest is
    /// the same vocabulary as the `:` line, because there is only one.
    #[arg(long, value_name = "COMMAND")]
    pub remote: Option<String>,

    /// Start even if another gmuse is already running.
    ///
    /// The lock exists because two instances both open the audio device and
    /// both write `session.toml`. Nothing stops you wanting a second one
    /// anyway — a second machine's session over SSH, say — so the refusal is a
    /// default, not a rule.
    #[arg(long)]
    pub no_lock: bool,
}

/// What gmuse can be asked to do instead of playing music.
#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Operations on the library itself.
    #[command(subcommand)]
    Library(Library),
}

#[derive(Debug, clap::Subcommand)]
pub enum Library {
    /// Apply a file of proposed facts.
    ///
    /// **A dry run unless given `--write`.** The genre pipeline already
    /// learned this one: a pass that writes by default is a pass nobody reads
    /// the output of first, and these files are produced by tools that are
    /// wrong often enough to be worth reviewing.
    Apply {
        /// The `.jsonl` file to read.
        file: PathBuf,
        /// Append the facts. Without this, nothing is written.
        #[arg(long)]
        write: bool,
    },
}

impl Cli {
    /// Where to write the session log, if at all.
    pub fn log_path(&self) -> Option<PathBuf> {
        self.log
            .clone()
            .map(|p| p.unwrap_or_else(crate::audit::default_path))
    }

    /// `--library` roots, with `~` expanded.
    ///
    /// A shell expands `~` before gmuse sees it, but an env var does not —
    /// `GMUSE_LIBRARY=~/Music` and a quoted `-L '~/Music'` both arrive
    /// literally, and a literal `~/Music` silently walks nothing.
    pub fn library_roots(&self) -> Vec<PathBuf> {
        self.library
            .iter()
            .map(|p| crate::config::expand_tilde(p))
            .collect()
    }

    pub fn player_config(&self) -> PlayerConfig {
        PlayerConfig {
            device: self.device.clone(),
            rate: self.rate,
            resample: self.resample,
            volume: self.volume,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Cli {
        let mut full = vec!["gmuse"];
        full.extend_from_slice(args);
        Cli::try_parse_from(full).unwrap_or_else(|e| panic!("{args:?}: {e}"))
    }

    /// `--log` shipped broken because nothing here was tested: clap rejected
    /// the bare flag at runtime, which no compile or unit test could see.
    /// A shell expands `~`, but an env var does not — `GMUSE_LIBRARY=~/Music`
    /// arrives literally, and a literal `~/Music` silently walks nothing.
    #[test]
    fn library_roots_expand_a_tilde() {
        let cli = Cli::parse_from(["gmuse", "-L", "~/Music", "-L", "/abs/path"]);
        let home = std::env::var("HOME").expect("HOME");
        assert_eq!(
            cli.library_roots(),
            vec![
                PathBuf::from(&home).join("Music"),
                PathBuf::from("/abs/path")
            ]
        );
    }

    #[test]
    fn log_takes_an_optional_path() {
        assert_eq!(parse(&[]).log_path(), None, "absent means no logging");

        let bare = parse(&["--log"]).log_path().expect("bare --log should log");
        assert!(
            bare.ends_with("gmuse/session.log"),
            "bare --log should use the default place, got {bare:?}"
        );

        assert_eq!(
            parse(&["--log", "/tmp/x.log"]).log_path(),
            Some(PathBuf::from("/tmp/x.log"))
        );
    }

    #[test]
    fn every_flag_parses() {
        let cli = parse(&[
            "--device",
            "built-in",
            "--rate",
            "48000",
            "--resample",
            "high",
            "--volume",
            "0.5",
            "--library",
            "/m",
            "--rescan",
            "--no-cache",
            "--no-config",
            "--no-lock",
            "--log",
            "/tmp/l.log",
        ]);
        assert_eq!(cli.device.as_deref(), Some("built-in"));
        assert_eq!(cli.rate, Some(48_000));
        assert_eq!(cli.volume, 0.5);
        assert!(cli.rescan && cli.no_cache && cli.no_config && cli.no_lock);
    }

    /// clap's own consistency check: duplicate short flags, bad defaults and
    /// the like are a panic at runtime otherwise.
    #[test]
    fn the_command_is_well_formed() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
