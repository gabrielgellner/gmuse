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
        /// **One row per copy rather than per recording** — #183.
        ///
        /// The default hands over the copy this device would open, which is
        /// right for a pass about the *performance*: an identification or a
        /// credit is true of the recording however many files hold it.
        ///
        /// A pass about the **bytes** wants this instead. `EncodingMeasured`
        /// is keyed by encoding, so a library holding one recording as both a
        /// FLAC and an MP3 — the shape #159 supports — could otherwise only
        /// ever measure whichever one `open` happened to pick, and the two
        /// answer different questions: a FLAC's cutoff says whether it is
        /// really lossless or a transcode in a lossless container, an MP3's
        /// says whether it delivers what its bitrate declares.
        #[arg(long)]
        every_copy: bool,
        /// **Narrow the list the way `:filter` narrows the browser** — #186.
        ///
        /// The same filter language, through the same parser, so a spec the
        /// `:` line accepts this accepts. The re-buy list #87 asks for is
        /// `--filter 'cutoff<17000 and cliff>2500' --every-copy`: a cutoff
        /// alone is not evidence, because a quiet track stops early without an
        /// encoder having stopped it — measured, that difference is 38 rows
        /// against 7.
        ///
        /// Asked of the copy each row names, which is why it pairs with
        /// `--every-copy`: the measurements are keyed by encoding, and a
        /// filter over one copy per recording is asking about whichever file
        /// `open` picked.
        #[arg(long, value_name = "EXPR")]
        filter: Option<String>,
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
    /// **Recordings this device holds more than one file of.**
    ///
    /// One performance, several encodings: a FLAC bought over an MP3 already
    /// held, a re-download, a friend's rip. That is a supported shape — one
    /// recording with two encodings is what `replace` settled on, and history
    /// hangs off the recording so it crosses whichever copy is kept — but
    /// nothing could list them.
    ///
    /// The gap that hid: `query` hands out **one locator per recording**, so
    /// the second copy is not on any work list. `audit duplicates` asks the
    /// neighbouring question — which *recordings* are the same music — and is
    /// structurally unable to see two copies of one recording, because it only
    /// ever sees the one locator. `verify` checks each file against its own
    /// hash and says nothing about siblings.
    ///
    /// So this is the one thing that answers "what do I hold twice", and the
    /// answer is a question for a person: which to keep, or whether to keep
    /// both, is not decidable from bitrate alone — a second copy can carry the
    /// better tags, as the `Early James` pair did.
    Copies,
    /// **Positions on a disc that more than one recording claims** — #168.
    ///
    /// A slot is a place on a medium and exactly one thing can be in it. But
    /// `Id::<Track>::of_track` mints a slot from **medium and recording**, not
    /// medium and position, and that is deliberate: keyed by position,
    /// correcting a tracklist re-mints every row that shifted and strands
    /// whatever hung off the old ones. Its own doc names the cost it accepted
    /// — the album that plays one recording twice, which folds to one slot.
    ///
    /// It did not anticipate the other direction. **Two recordings claiming one
    /// position** mint two slots, and nothing notices. That is what cataloguing
    /// 487 MusicBrainz tracklists produced here: 202 positions across 77 media,
    /// each a place where the library already believed something and the
    /// tracklist said otherwise.
    ///
    /// It is almost always a misidentification, and the shape is legible. Our
    /// `Adele/21/11 Someone Like You.mp3` runs 295.4s; MusicBrainz's live
    /// Royal Albert Hall recording runs 294.0s and the studio one 285.0s, so
    /// an identification matching on length took the live one by 1.4 seconds.
    /// The tracklist then placed the studio recording in the same slot, and
    /// the row nobody can play is the one that is right.
    ///
    /// **Reported, never decided.** A disc really can play one recording twice
    /// and a compilation really can carry two versions, so what to do is a
    /// person's call. What this gives them is the position, both claimants,
    /// and which of them this device can actually open.
    Slots {
        /// **Hand a tool the copies to listen to** — #169's neighbour.
        ///
        /// A contested position is exactly the case a name cannot settle:
        /// both claimants are called the same thing, both are on the same
        /// record, and the only difference is which audio is in the seat.
        /// Measured on the first run over this library, duration alone left
        /// 36 of 80 undecidable and a fingerprint settled 36 of them.
        ///
        /// Emits the same rows `query` does, narrowed to the copies sitting
        /// at a contested position, so `identify fingerprint` reads it with
        /// no new vocabulary.
        #[arg(long)]
        work: bool,
    },
    /// **Does every file still hash to what the log says it is?**
    ///
    /// Identity here is the audio, so the library's whole account of itself
    /// rests on one claim per copy: these bytes produce this `AudioId`. Nothing
    /// checked it. A file replaced in place, a re-encode written over its
    /// original, a copy that came back wrong off a backup — all of them leave
    /// the log describing bytes that are no longer there, and every id derived
    /// from those bytes silently names the wrong thing.
    ///
    /// **Reads only, and reads everything.** About twenty seconds for seven
    /// thousand files, because each one is decoded far enough to hash. That is
    /// cheap for the question, which is whether anything the library says is
    /// still true.
    ///
    /// A file on disk that no holding names is not reported here — that is
    /// `import`'s offer, and a different question.
    Verify,
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
    /// **Bring a retired copy back** — `storage-layout.md` §6.
    ///
    /// The other half of `remove`, and `Fact::EncodingRestored` has existed
    /// since it was written: *"a step you cannot take back is one nobody
    /// takes, and §6's whole design is that retiring is reversible and purging
    /// is not."* Nothing could emit it, so the reversibility was a promise the
    /// vocabulary made and the command line could not keep.
    ///
    /// **The bytes move first, then this.** A retirement puts them under
    /// `.departed` keeping the path they had, so a restore is a move back —
    /// and this lifts the tombstone off what is now there. Naming a file the
    /// library never retired is reported, not guessed at.
    Restore {
        /// Files or folders to bring back
        paths: Vec<PathBuf>,
        /// Lift the tombstones. Without this, nothing happens
        #[arg(long)]
        write: bool,
    },
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
    /// **What the tree would draw** — the levels, without a terminal (#182).
    ///
    /// `:view levels genre@1,album` reshapes the browser, and until now that
    /// sentence could only be said *to* the TUI and only be answered *by*
    /// looking at it. So the one thing a level does — decide which rows fall
    /// under which heading — had no reading outside the thing being read.
    ///
    /// Read-only, and it groups exactly what the browser groups: same
    /// `Tree::grouped`, same member set, same smart artist sort the UI starts
    /// with. A disagreement between this and the pane is a bug in one of them
    /// rather than a difference in how they were asked.
    Tree {
        /// `outer,inner` — the two fields to group by, as `:view levels`
        /// takes them. Empty is the default pair, `file_under,album`.
        ///
        /// A level may name a depth: `genre@1` is the broadest ancestor,
        /// `genre@2` the one under it (#79).
        #[arg(long, value_name = "SPEC", default_value = "")]
        levels: String,
        /// Print each album's tracks too, not just the two headings.
        #[arg(long)]
        tracks: bool,
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
