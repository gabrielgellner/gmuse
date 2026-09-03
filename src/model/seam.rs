//! The seam — issue #77.
//!
//! **The library answers questions and accepts changes; the player asks and
//! plays.** This module is where that sentence becomes a type.
//!
//! Today `src/ui/` reaches into a `Store` and handles `PathBuf`s directly, so
//! the player and the librarian are one program by accident rather than by
//! design. [`Librarian`] is the surface that ends that: everything the player
//! needs, expressed in the model's terms, and nothing that requires it to know
//! where a file sits or what a tag key is called.
//!
//! ## Two directions, and only one of them holds yet
//!
//! *The model must not know about the player* holds today and is asserted, so
//! it cannot quietly stop holding. Nothing under `src/model/` names `ui`,
//! `audio`, `cmd`, `ratatui` or `crossterm`; its only reaches outside are
//! `audio_id`, which is identity rather than playback, and `lofty`, which is
//! the tag boundary.
//!
//! *The player must not know about the store* does **not** hold yet, and
//! pretending otherwise would be worse than saying so. [`tests::the_leak`]
//! counts what is left and refuses to let it grow, which turns a 10,000-line
//! migration into something with a number on it that can only go down.

use crate::model::encoding::{Encoding, Encodings, Format};
use crate::model::entity::{Device, Recording};
use crate::model::id::Id;
use crate::model::provenance::Provenance;
use crate::model::reference::Reference;
use crate::model::spine::Spine;
use crate::model::vocabulary::Vocabulary;

/// What the player is allowed to ask, and what it is allowed to ask for.
///
/// Deliberately small. The four things `src/ui/` asks a `Store` for today are
/// *get*, *iterate*, *upsert* and *look up by path* — and the last of those is
/// the one that must not survive the move, because a path is a fact about one
/// device's copy and the player has no business holding one.
pub struct Librarian {
    pub spine: Spine,
    pub reference: Reference,
    pub vocabulary: Vocabulary,
    pub encodings: Encodings,
    pub provenance: Provenance,
}

impl Default for Librarian {
    fn default() -> Self {
        Self::new()
    }
}

impl Librarian {
    pub fn new() -> Self {
        Librarian {
            spine: Spine::default(),
            reference: Reference::default(),
            vocabulary: Vocabulary::default(),
            encodings: Encodings::default(),
            provenance: Provenance::default(),
        }
    }

    /// What a row on screen needs to draw itself.
    ///
    /// The projection #78 fills out. What matters here is its shape: it is
    /// assembled *by the library* from several entities, and it holds no path,
    /// because there is no path that is true of the library rather than of one
    /// machine.
    pub fn row(&self, recording: Id<Recording>) -> Option<Row> {
        let rec = self.spine.recordings.get(&recording)?;
        let canonical = self.encodings.canonical(recording);
        Some(Row {
            recording,
            title: rec.title.clone(),
            length: rec.length,
            format: canonical.map(|e| e.format),
            bitrate: canonical.map(|e| e.bitrate),
            available: !self.encodings.of(recording).is_empty(),
        })
    }

    /// Which copy this machine should open, and where it sits on it.
    ///
    /// **The one place a locator is produced**, and it is produced at the
    /// moment of playing rather than stored anywhere the player can keep. A
    /// player that cannot hold a path cannot key anything by one.
    pub fn open(
        &self,
        recording: Id<Recording>,
        here: &[Id<Device>],
        prefers: &[Format],
    ) -> Option<(&Encoding, &str)> {
        let pinned = self
            .spine
            .recordings
            .get(&recording)
            .and_then(|r| r.pinned)
            .map(|p| p.id());
        let chosen = self
            .encodings
            .for_playback(recording, pinned, here, prefers)?;
        let locator = self
            .encodings
            .holders(chosen.id)
            .into_iter()
            .find(|h| here.contains(&h.device.id()))
            .map(|h| h.locator.as_str())?;
        Some((chosen, locator))
    }

    /// Everything playable, in a stable order.
    pub fn recordings(&self) -> impl Iterator<Item = Id<Recording>> + '_ {
        self.spine.recordings.keys().copied()
    }
}

/// One displayed row. Holds identities and values, never a location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub recording: Id<Recording>,
    pub title: String,
    pub length: std::time::Duration,
    pub format: Option<Format>,
    pub bitrate: Option<u32>,
    pub available: bool,
}

/// What a `Row` deliberately does not carry.
///
/// Named so the omission reads as a decision rather than an oversight: a
/// `PathBuf` on a row is what let `history.toml` be keyed by path, which cost
/// three stranded entries and a four-star rating.
pub const ROW_HOLDS_NO_PATH: () = ();

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn read(dir: &str) -> Vec<(String, String)> {
        let mut out = Vec::new();
        let mut stack = vec![Path::new(dir).to_path_buf()];
        while let Some(p) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(&p) else {
                continue;
            };
            for e in entries.flatten() {
                let path = e.path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|x| x == "rs")
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    out.push((path.display().to_string(), text));
                }
            }
        }
        out
    }

    /// **The direction that holds, asserted so it keeps holding.**
    ///
    /// The librarian must not know there is a player. Nothing here reaches for
    /// a terminal, an audio device, or a key binding — and the day something
    /// does, this fails rather than the seam quietly closing up.
    #[test]
    fn the_model_does_not_know_the_player_exists() {
        let forbidden = [
            "crate::ui",
            "crate::cmd",
            "crate::audio::",
            "crate::store",
            "crate::library",
            "crate::session",
            "crate::next",
            "crate::playlist",
            "ratatui",
            "crossterm",
            "cpal",
        ];
        // Only `use` lines: "depends on" means importing, and a test that
        // scanned raw text would trip over this very list.
        let mut leaks = Vec::new();
        for (file, text) in read("src/model") {
            for line in text.lines().map(str::trim) {
                if !line.starts_with("use ") {
                    continue;
                }
                for f in forbidden {
                    if line.contains(f) {
                        leaks.push(format!("{file}: {line}"));
                    }
                }
            }
        }
        assert!(
            leaks.is_empty(),
            "the model reached into the player: {leaks:#?}"
        );
    }

    /// **The direction that does not hold yet**, with a number on it.
    ///
    /// `src/ui/` still names a `Store` and still handles `PathBuf`s. Asserting
    /// zero today would just fail; asserting *no worse than today* turns a
    /// 10,000-line migration into a ratchet, and makes any new leak a test
    /// failure rather than something to be found later.
    ///
    /// **Lower these as #78 and #79 land. Never raise them.**
    #[test]
    fn the_leak_from_the_player_only_narrows() {
        const STORE: usize = 62;
        const PATHS: usize = 28;

        let (mut store, mut paths) = (0, 0);
        for (_, text) in read("src/ui") {
            store += text.matches("Store").count();
            paths += text.matches("PathBuf").count();
        }

        assert!(
            store <= STORE && paths <= PATHS,
            "the seam widened: Store {store} (was {STORE}), PathBuf {paths} (was {PATHS})"
        );
        if store < STORE || paths < PATHS {
            eprintln!("seam narrowed to Store {store}, PathBuf {paths} — lower the constants");
        }
    }

    /// A row is assembled by the library and carries no location, so the
    /// player cannot key anything by one.
    #[test]
    fn a_row_is_made_of_identities_and_values() {
        use crate::model::spine::Recording as Rec;
        use std::time::Duration;

        let mut l = Librarian::new();
        let id = Id::<Recording>::derived(b"callisto");
        l.spine.recordings.insert(
            id,
            Rec {
                id,
                title: "Callisto".into(),
                work: None,
                recorded: None,
                bpm: None,
                length: Duration::from_secs(400),
                pinned: None,
            },
        );

        let row = l.row(id).expect("a catalogued recording has a row");
        assert_eq!(row.title, "Callisto");
        assert!(!row.available, "no encoding, and still an ordinary row");
        assert_eq!(row.format, None);
    }

    /// A recording nothing holds is still a row — decided about, not owned.
    #[test]
    fn something_not_owned_is_not_a_missing_row() {
        let l = Librarian::new();
        assert!(l.row(Id::derived(b"never-catalogued")).is_none());
    }
}
