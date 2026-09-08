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

    /// Shelf root to scan. Repeat for several. Remembered in the cache, so
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
    Shelf(Shelf),
}

#[derive(Debug, clap::Subcommand)]
pub enum Shelf {
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
    /// Hand a tool the work list: an id and a locator per row.
    ///
    /// The other half of the contract `apply` completes. A tool opens the
    /// **locator** and proposes against the **id**, so it never computes an
    /// identity and never keys anything by a path.
    Query {
        /// Only recordings resting on their bytes — the ones no authority has
        /// named, and the only ones an identification pass should spend a rate
        /// limit on.
        #[arg(long)]
        unidentified: bool,
    },
    /// **Catalogue this audio — the declaration a scan no longer makes** (#62).
    ///
    /// `library-api.md` §2: a file appearing in a watched root is noticed, not
    /// catalogued. This is how it becomes part of the library, and it is a
    /// person saying so rather than a walk inferring it.
    ///
    /// **A dry run unless given `--write`**, like `apply`, and for the same
    /// reason: this one reads tags, and what it reads becomes claims in the
    /// log.
    ///
    /// With no paths it imports everything the last scan reported as unknown,
    /// which is the common case — "yes, those, I meant those".
    Import {
        /// **The collection this music is being brought in as** — #94.
        ///
        /// A declaration, said at the moment the declaring happens. Bringing
        /// music in and saying what it is *for* are one act, and splitting
        /// them means every import is followed by a second pass that has to
        /// re-derive from the folder what the person already knew — which is
        /// the inference #90 and #94 took away.
        ///
        /// The same shape a package manager gives a dependency group: you say
        /// which section you are installing into, once, rather than sorting it
        /// out afterwards.
        ///
        /// Optional, because a collection is a judgement and not every import
        /// carries one.
        #[arg(long, value_name = "NAME")]
        into: Option<String>,
        /// Files or folders to catalogue. Empty means whatever the library
        /// roots hold that is not known yet.
        paths: Vec<PathBuf>,
        /// **Take files no authority names** — #160, and the escape hatch the
        /// gate needs to be honest.
        ///
        /// Without this, a file carrying no MusicBrainz id is refused rather
        /// than catalogued on its own bytes. `library-api.md` §7 states that
        /// rule; the code did the opposite of it until this existed, and 479
        /// recordings arrived resting on their bytes in a single import
        /// because nothing ever asked.
        ///
        /// **The hatch is not a formality.** A white label, a live rip, a
        /// friend's record — MusicBrainz has never heard of any of them and
        /// they belong here. What changes is that taking one is something you
        /// say, and the refusal above it names what a lookup would need.
        #[arg(long)]
        unidentified: bool,
        /// Append the facts. Without this, nothing is written.
        #[arg(long)]
        write: bool,
    },
    /// **Say where every file belongs, and what stands in the way** — #132.
    ///
    /// One readable tree, rendered from the library rather than from the tags:
    /// `{file_under}/{album}/{disc-}{track} {title}.{ext}`. Structural facts
    /// only — never genre, era or catalog, because those are views.
    ///
    /// **A dry run unless given `--write`.** A path that is illegal, too long,
    /// or wanted by two copies goes to the list rather than being mangled, and
    /// the file stays where it is. That is safe because the path is not the
    /// binding — identity is the audio.
    Render {
        /// Actually move the files. Without this, nothing is touched.
        #[arg(long)]
        write: bool,
    },
    /// Propose filing whatever is in no collection, by the folder it sits in.
    ///
    /// **A proposal, printed, and never applied.** The folder is evidence a
    /// person left, and reading it once is the whole of what it is for: a
    /// scan that filed by folder would make the filesystem authoritative
    /// again, which is exactly what #90 and #94 took away.
    ///
    /// Review it, then `gmuse shelf apply --write`.
    Unfiled,
    /// **Albums the library holds more than once** — #156.
    ///
    /// A group's id is a function of what the tags said, so learning an
    /// album's MusicBrainz id changes its identity and `import` mints a second
    /// album beside the first. Nothing else in this tool prints a group id, so
    /// without this the merge that repairs it cannot be written by hand.
    ///
    /// **Reported, never decided.** Two albums sharing a name are not always
    /// one record: `AC/DC — High Voltage` is the 1975 Australian and the 1976
    /// international, and they are correctly two. So this prints what it found
    /// and how many files sit on each, and a person says which is the winner.
    Duplicates,
    /// **Artists the library holds more than once** — `merge.rs`, and the
    /// split `of_artist` documents as the price of its name fallback.
    ///
    /// An artist is minted from its MusicBrainz id where the tags carry one
    /// and from its name otherwise, so one artist arriving both ways mints
    /// two. Of 1015 named artists here, **82 names are held by more than one
    /// id** — and almost none of it shows: `line_of` dedupes by rendered
    /// name, so two ids that both say `Daft Punk` draw as one. Only where the
    /// casing also differs does the split reach a folder.
    ///
    /// **Reported with a direction, because here there is a rule.** Unlike a
    /// duplicate album, the winner is not a judgement call: the
    /// MusicBrainz-derived id is the higher rung. The line is still printed
    /// for a person to read, and a pair where *both* ids came from
    /// MusicBrainz gets no line at all — two MusicBrainz artists sharing a
    /// name is evidence for two.
    Artists,
    /// **Retire these copies** — `storage-layout.md` §6, #159.
    ///
    /// How a file leaves, and it is two steps because the first is reversible
    /// and the second is not: this writes a tombstone and moves the bytes to
    /// `.departed/`, keeping the path they had beneath it. **Nothing is
    /// deleted.** A purge is a separate operation and does not exist.
    ///
    /// The recording stays, and so does everything said about it — a row the
    /// library holds no copy of is ordinary, and a track you do not own is a
    /// row on purpose (#155). What stops is the library claiming a *copy* it
    /// has retired.
    ///
    /// **A file already under `.departed` is accepted**, because that is the
    /// album somebody retired by hand before this existed, and the tombstone
    /// is the only thing it is missing.
    Remove {
        /// Files or folders to retire.
        paths: Vec<PathBuf>,
        /// Write the tombstones and move the bytes. Without this, nothing
        /// happens.
        #[arg(long)]
        write: bool,
    },
    /// **Artists whose name is in doubt** — #154.
    ///
    /// Two shapes, and they are the two ends of one guard. An artist with a
    /// right id and **no name** is what #145 leaves behind when it withholds a
    /// joined credit — correct, and it makes the artist unreachable in a
    /// browse. An artist named **more than one thing** is where that guard did
    /// not fire: `ArtistNamed` is last-writer-wins, so the last file folded
    /// owns the label, and a joined credit that slipped through renames the
    /// artist it merely starts with.
    ///
    /// Nothing else prints an artist id, so without this neither is fixable by
    /// hand.
    Names,
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
    /// **`import` is a dry run unless told otherwise** — #62, and the same
    /// rule `apply` follows: this one reads tags, and what it reads becomes
    /// claims in the log.
    #[test]
    fn import_writes_nothing_unless_asked() {
        let bare = parse(&["shelf", "import", "/m/a.flac"]);
        let Some(Command::Shelf(Shelf::Import { paths, write, .. })) = bare.command else {
            panic!("not an import: {:?}", bare.command)
        };
        assert_eq!(paths, vec![PathBuf::from("/m/a.flac")]);
        assert!(!write, "a bare import would have written");

        let asked = parse(&["shelf", "import", "--write", "/m/a.flac", "/m/b.flac"]);
        let Some(Command::Shelf(Shelf::Import { paths, write, .. })) = asked.command else {
            panic!("not an import")
        };
        assert_eq!(paths.len(), 2);
        assert!(write);

        // **No paths is the common case**: import whatever the scan said it
        // did not recognise, which is the answer to the offer it made.
        let all = parse(&["shelf", "import"]);
        let Some(Command::Shelf(Shelf::Import { paths, .. })) = all.command else {
            panic!("not an import")
        };
        assert!(paths.is_empty());
    }

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
