//! Catalogs: several bodies of music, one or more active at a time.
//!
//! A catalog is a **membership rule**, not a second library. DESIGN §4 already
//! makes `Shelf` a projection over a subset of the `Scan`, so confining that
//! subset confines browsing, shuffle and `aaa_mode` with it — `next()` needs no
//! knowledge of catalogs at all.
//!
//! What catalogs deliberately do **not** touch: playlists and the queue hold
//! `CopyId`s straight into the `Scan`, so they cross catalog boundaries
//! freely. That is the point of a playlist.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};

use crate::model::rows::CopyId;

/// The name used when tracks are scanned without any catalog claiming them —
/// `--library` on the command line, or a config with no `[catalogs]` at all.
pub const DEFAULT: &str = "default";

/// One named body of music.
///
/// Membership is `collection` **and** `where`, either of which may be absent,
/// and a catalog with neither holds everything. §7 settled that a catalog is a
/// named, persistent filter; what #94 settled is that **neither half of it may
/// read a path**. A collection is a stored fact about which recordings belong,
/// so it survives a move and means the same thing on another machine — which
/// is exactly what `roots` could not do.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub name: String,
    /// A collection whose members belong — by name, resolved to its id.
    ///
    /// **What replaced `roots`** (#90, #94). A catalog used to say "everything
    /// under this folder", which made membership a fact about where a file
    /// sits: moving it changed what it belonged to, and the rule could not
    /// cross a machine. Measured on the real library, no genre rule reproduces
    /// `~/MusicLibrary/Dance` either — every one of its 790 files carries an
    /// `Electronic` term and so do 876 in `General`, which are Laurie
    /// Anderson, Beck, Enya and DEVO. The folder was recording a judgement,
    /// and a collection is where a judgement lives.
    pub collection: Option<String>,
    /// An expression the track must also match (§7).
    ///
    /// A `where`-only catalog adds nothing to *scan* — it narrows what has
    /// already been indexed, so the files still have to be reachable from
    /// `library`.
    pub filter: Option<crate::filter::Filter>,
}

impl Catalog {
    /// Whether a track belongs. Both halves must hold, and an absent half is
    /// simply not a constraint — so `roots` alone, `where` alone, and both
    /// together all read the same way.
    fn contains(
        &self,
        subject: &crate::model::rows::Subject<'_>,
        env: crate::filter::Env<'_>,
    ) -> bool {
        // **No membership decision reads a path** (#94). It used to: a root
        // matched the file's whereabouts, so moving a file changed what it
        // belonged to and the rule meant nothing on another machine. What is
        // left here is the predicate; the collection half is decided by
        // `Catalogs::named`, which has the library to ask.
        match &self.filter {
            Some(f) => f.compile(env)(subject),
            None => true,
        }
    }

    /// What the config said, for view 6.
    pub fn rule_label(&self) -> String {
        let mut parts = Vec::new();
        if let Some(c) = &self.collection {
            parts.push(format!("collection {c}"));
        }
        if let Some(f) = &self.filter {
            parts.push(f.source.clone());
        }
        parts.join("  &  ")
    }
}

/// Which catalogs are in force.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Active {
    #[default]
    All,
    Only(BTreeSet<String>),
}

/// How `:catalog` was asked to change things.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    All,
    /// Replace the active set.
    Only(Vec<String>),
    /// Add or remove, leaving the rest alone.
    Adjust(Vec<(bool, String)>),
}

#[derive(Debug, Clone, Default)]
pub struct Catalogs {
    defined: Vec<Catalog>,
    active: Active,
}

impl Catalogs {
    pub fn active(&self) -> &Active {
        &self.active
    }

    /// Adopt a remembered choice, ignoring names that no longer exist.
    pub fn set_active(&mut self, active: Active) {
        self.active = match active {
            Active::All => Active::All,
            Active::Only(names) => {
                let known: HashSet<&str> = self.names().into_iter().collect();
                Active::Only(
                    names
                        .into_iter()
                        .filter(|n| known.contains(n.as_str()))
                        .collect(),
                )
            }
        };
    }

    pub fn defined(&self) -> &[Catalog] {
        &self.defined
    }

    /// Whether a named catalog is currently in force. `Active::All` means all
    /// of them, which is not the same as none being active.
    pub fn is_active(&self, name: &str) -> bool {
        match &self.active {
            Active::All => true,
            Active::Only(names) => names.contains(name),
        }
    }

    pub fn names(&self) -> Vec<&str> {
        self.defined.iter().map(|c| c.name.as_str()).collect()
    }

    /// Add a catalog, or refine one that already exists.
    pub fn define(
        &mut self,
        name: &str,
        collection: Option<String>,
        filter: Option<crate::filter::Filter>,
    ) {
        if let Some(existing) = self.defined.iter_mut().find(|c| c.name == name) {
            if collection.is_some() {
                existing.collection = collection;
            }
            if filter.is_some() {
                existing.filter = filter;
            }
        } else {
            self.defined.push(Catalog {
                name: name.to_string(),
                collection,
                filter,
            });
        }
    }

    // **`all_roots` lived here and is gone** (#90, #94).
    //
    // A catalog had roots, and scanning covered all of them regardless of what was
    // active. Two jobs in one field: *where to look for files* and *what belongs
    // to this catalog*. The second was the one that had to go — membership by path
    // means moving a file changes what it belongs to, and the rule means nothing
    // on another machine.
    //
    // The first job stays where it always belonged: `library` in the config, which
    // is the only thing that says where the music is now.

    /// Names currently in force, for display.
    pub fn active_names(&self) -> Vec<&str> {
        match &self.active {
            Active::All => self.names(),
            Active::Only(names) => self
                .defined
                .iter()
                .filter(|c| names.contains(&c.name))
                .map(|c| c.name.as_str())
                .collect(),
        }
    }

    /// A short label for the status line. `None` when everything is in force
    /// and there is nothing worth saying.
    pub fn label(&self) -> Option<String> {
        if self.defined.len() < 2 {
            return None;
        }
        match &self.active {
            Active::All => Some("all".into()),
            Active::Only(names) if names.is_empty() => Some("none".into()),
            Active::Only(_) => Some(self.active_names().join("+")),
        }
    }

    /// Apply a `:catalog` request. Returns an error message for names that do
    /// not exist, rather than silently doing nothing.
    pub fn apply(&mut self, spec: Spec) -> Result<(), String> {
        let known: HashSet<&str> = self.names().into_iter().collect();
        let check = |name: &str| -> Result<(), String> {
            if known.contains(name) {
                Ok(())
            } else {
                Err(format!("no catalog called {name:?}"))
            }
        };

        match spec {
            Spec::All => self.active = Active::All,
            Spec::Only(names) => {
                for name in &names {
                    check(name)?;
                }
                self.active = Active::Only(names.into_iter().collect());
            }
            Spec::Adjust(deltas) => {
                for (_, name) in &deltas {
                    check(name)?;
                }
                // Adjusting from "all" means starting from everything, which is
                // the only reading of `:catalog -jazz` that makes sense.
                let mut set: BTreeSet<String> = match &self.active {
                    Active::All => self.names().into_iter().map(String::from).collect(),
                    Active::Only(names) => names.clone(),
                };
                for (add, name) in deltas {
                    if add {
                        set.insert(name);
                    } else {
                        set.remove(&name);
                    }
                }
                self.active = Active::Only(set);
            }
        }
        Ok(())
    }

    /// One catalog's membership, by name.
    ///
    /// **The composed view asks for a term rather than for "the active set"**
    /// (#94). Which catalogs are active stopped being a property of the
    /// catalogs the moment the operator left the noun: `general or dance` is a
    /// stack, and `Active` is what it used to be spelled as.
    pub fn named(
        &self,
        name: &str,
        rows: &dyn crate::model::rows::Listing,
        all: &[CopyId],
        env: crate::filter::Env<'_>,
    ) -> Vec<CopyId> {
        let Some(catalog) = self.defined.iter().find(|c| c.name == name) else {
            // A stack naming a catalog that no longer exists selects nothing,
            // which is what a renamed catalog should look like — empty and
            // visible, rather than silently the whole library.
            return Vec::new();
        };
        // The collection half needs the library rather than the row, so it is
        // resolved here — once, not per row.
        let members: Option<std::collections::HashSet<_>> = catalog.collection.as_ref().map(|c| {
            let id = crate::model::id::Id::<crate::model::entity::Collection>::of_name(c);
            rows.entries_of(id).into_iter().collect()
        });

        all.iter()
            .copied()
            .filter(|id| {
                members
                    .as_ref()
                    .is_none_or(|m| rows.recording_of(*id).is_some_and(|r| m.contains(&r)))
            })
            .filter(|id| rows.subject(*id).is_some_and(|s| catalog.contains(&s, env)))
            .collect()
    }

    /// The tracks the library should be built from.
    ///
    /// With nothing defined, or everything active, this is the whole store —
    /// so a user who never configures a catalog sees no change at all.
    pub fn members(
        &self,
        rows: &dyn crate::model::rows::Listing,
        all: &[CopyId],
        env: crate::filter::Env<'_>,
    ) -> HashSet<CopyId> {
        if self.defined.is_empty() || matches!(self.active, Active::All) {
            return all.iter().copied().collect();
        }
        let Active::Only(names) = &self.active else {
            unreachable!("Active::All is handled above")
        };
        // **Through `named`, so there is one membership rule and not two.**
        // The active set had its own copy of it, which is how a catalog
        // defined by a collection came back holding the whole library: the
        // copy only knew about the filter half (#94).
        names
            .iter()
            .flat_map(|name| self.named(name, rows, all, env))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Tagged as Track;
    use crate::model::entity::Collection;
    use crate::model::id::Id;
    use crate::model::ord::OrdKey;
    use crate::model::rows::Listing;
    use crate::scan::Holdings as Scan;
    use std::path::PathBuf;

    fn store_with(paths: &[&str]) -> Scan {
        let mut s = Scan::new();
        for p in paths {
            s.upsert(Track {
                path: PathBuf::from(p),
                ..Default::default()
            });
        }
        s
    }

    /// A library whose recordings have been put into collections by name.
    ///
    /// The two steps are separate because they are separate in life: a scan
    /// finds files, and somebody decides what belongs where. Nothing about the
    /// path takes part in the second step, which is the point of #94.
    fn collected(held: &Scan, into: &[(&str, &[&str])]) -> crate::model::seam::Librarian {
        let bare = crate::fixture::from_holdings(held);
        let mut i = crate::model::instance::Instance::new(crate::fixture::device());
        for (name, members) in into {
            let id = Id::<Collection>::of_name(name);
            i.name_collection(100, id, name, false);
            let mut key: Option<OrdKey> = None;
            for path in *members {
                // By locator, not by re-deriving an id: a fixture may pin its
                // own audio id, and this helper should not care which.
                let copy = Listing::rows(&bare)
                    .iter()
                    .copied()
                    .find(|id| Listing::playable(&bare, *id).is_some_and(|p| p.locator == *path))
                    .expect("scanned");
                let recording = Listing::recording_of(&bare, copy).expect("scanned");
                let k = OrdKey::between(key.as_ref(), None);
                i.add_to_collection(100, id, recording, k.clone());
                key = Some(k);
            }
        }
        crate::fixture::from_holdings_and(held, i.records())
    }

    fn fixture() -> (Scan, crate::model::seam::Librarian, Catalogs) {
        let held = store_with(&[
            "/m/a.mp3",
            "/m/b.mp3",
            "/m/c.mp3",
            "/m/d.mp3",
            "/m/loose.mp3",
        ]);
        let l = collected(
            &held,
            &[
                ("General", &["/m/a.mp3", "/m/b.mp3"]),
                ("Electronic", &["/m/c.mp3"]),
                ("Classical", &["/m/d.mp3"]),
            ],
        );
        let mut catalogs = Catalogs::default();
        catalogs.define("general", Some("General".into()), None);
        catalogs.define("electronic", Some("Electronic".into()), None);
        catalogs.define("classical", Some("Classical".into()), None);
        (held, l, catalogs)
    }

    /// Where each of these rows is held, for readable assertions.
    fn paths(l: &crate::model::seam::Librarian, ids: &HashSet<CopyId>) -> BTreeSet<String> {
        ids.iter()
            .map(|id| {
                Listing::playable(l, *id)
                    .expect("held here")
                    .locator
                    .to_string()
            })
            .collect()
    }

    fn members(l: &crate::model::seam::Librarian, catalogs: &Catalogs) -> HashSet<CopyId> {
        catalogs.members(l, &Listing::rows(l), crate::filter::Env::default())
    }

    #[test]
    fn no_catalogs_means_every_track() {
        let held = store_with(&["/m/a.mp3", "/m/b.mp3"]);
        let l = crate::fixture::from_holdings(&held);
        let catalogs = Catalogs::default();
        assert_eq!(members(&l, &catalogs).len(), 2);
        // And nothing to say about it in the status line.
        assert_eq!(catalogs.label(), None);
    }

    #[test]
    fn all_includes_tracks_no_catalog_claims() {
        let (_, l, catalogs) = fixture();
        // "/m/loose.mp3" is in no collection, but `all` must still show it, or
        // a file nobody has filed yet would be music that vanished.
        assert_eq!(members(&l, &catalogs).len(), 5);
    }

    #[test]
    fn only_one_catalog_narrows_the_library() {
        let (_, l, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        assert_eq!(
            paths(&l, &members(&l, &catalogs)),
            BTreeSet::from(["/m/a.mp3".to_string(), "/m/b.mp3".to_string()])
        );
        assert_eq!(catalogs.label().as_deref(), Some("general"));
    }

    #[test]
    fn a_subset_is_the_union() {
        let (_, l, mut catalogs) = fixture();
        catalogs
            .apply(Spec::Only(vec!["general".into(), "classical".into()]))
            .unwrap();
        assert_eq!(members(&l, &catalogs).len(), 3);
        // Definition order, not alphabetical: the label reads the way the
        // config was written.
        assert_eq!(catalogs.label().as_deref(), Some("general+classical"));
    }

    #[test]
    fn adjusting_from_all_starts_from_everything() {
        let (_, l, mut catalogs) = fixture();
        // `:catalog -classical` with everything active means "all but that".
        catalogs
            .apply(Spec::Adjust(vec![(false, "classical".into())]))
            .unwrap();
        let held = members(&l, &catalogs);
        assert!(!paths(&l, &held).contains("/m/d.mp3"));
        assert_eq!(held.len(), 3, "general (2) + electronic (1)");
    }

    #[test]
    fn adjusting_adds_and_removes() {
        let (_, l, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        catalogs
            .apply(Spec::Adjust(vec![(true, "classical".into())]))
            .unwrap();
        assert_eq!(members(&l, &catalogs).len(), 3);
        catalogs
            .apply(Spec::Adjust(vec![(false, "general".into())]))
            .unwrap();
        assert_eq!(members(&l, &catalogs).len(), 1);
    }

    #[test]
    fn an_unknown_name_is_an_error_and_changes_nothing() {
        let (_, l, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        let before = members(&l, &catalogs).len();

        let err = catalogs
            .apply(Spec::Only(vec!["general".into(), "nosuch".into()]))
            .unwrap_err();
        assert!(err.contains("nosuch"), "{err}");
        assert_eq!(
            members(&l, &catalogs).len(),
            before,
            "a bad name must not half-apply"
        );
    }

    #[test]
    fn none_active_is_allowed_and_says_so() {
        let (_, l, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec![])).unwrap();
        assert!(members(&l, &catalogs).is_empty());
        assert_eq!(catalogs.label().as_deref(), Some("none"));
    }

    /// **No membership decision reads a path** — the requirement of #94.
    ///
    /// The same recording, filed in a collection, then found somewhere else
    /// entirely: a different folder, a different drive, a name that no longer
    /// looks like the old one. It belongs to exactly what it belonged to
    /// before, because what it belongs to was never a fact about where it sat.
    ///
    /// The old rule failed this by construction. `roots = ["/m/dance"]` made
    /// membership a property of the filesystem, so tidying a folder silently
    /// emptied a catalog, and the same config on a second machine — where the
    /// music lives under another prefix — selected nothing at all.
    #[test]
    fn moving_a_file_does_not_change_what_it_belongs_to() {
        let audio = crate::audio_id::AudioId {
            scheme: 1,
            hash: 0xbeef,
        };
        let at = |path: &str| {
            let mut s = Scan::new();
            s.upsert(Track {
                path: PathBuf::from(path),
                audio_id: Some(audio),
                ..Default::default()
            });
            s
        };

        let mut catalogs = Catalogs::default();
        catalogs.define("dance", Some("Dance".into()), None);
        catalogs.apply(Spec::Only(vec!["dance".into()])).unwrap();

        let before = at("/m/dance/x.mp3");
        let l = collected(&before, &[("Dance", &["/m/dance/x.mp3"])]);
        assert_eq!(members(&l, &catalogs).len(), 1);

        // Reshelved. Nothing was decided about it, only moved.
        let after = at("/elsewhere/2003/x.mp3");
        let mut i = crate::model::instance::Instance::new(crate::fixture::device());
        let id = Id::<Collection>::of_name("Dance");
        i.name_collection(100, id, "Dance", false);
        let copy = Id::<crate::model::entity::Encoding>::of_audio(&audio);
        let recording = Listing::recording_of(&l, copy).expect("known before the move");
        i.add_to_collection(100, id, recording, OrdKey::between(None, None));
        let records = i.records().clone();
        let l = crate::fixture::from_holdings_and(&after, &records);
        assert_eq!(
            paths(&l, &members(&l, &catalogs)),
            BTreeSet::from(["/elsewhere/2003/x.mp3".to_string()]),
            "membership followed the folder instead of the recording"
        );
    }

    #[test]
    fn defining_the_same_name_twice_refines_it() {
        let mut catalogs = Catalogs::default();
        catalogs.define("mixed", Some("A".into()), None);
        catalogs.define("mixed", Some("B".into()), None);
        assert_eq!(catalogs.defined().len(), 1);
        assert_eq!(catalogs.defined()[0].collection.as_deref(), Some("B"));
    }

    #[test]
    fn a_track_in_two_catalogs_appears_once() {
        let held = store_with(&["/m/a.mp3"]);
        let l = collected(&held, &[("One", &["/m/a.mp3"]), ("Two", &["/m/a.mp3"])]);
        let mut catalogs = Catalogs::default();
        catalogs.define("one", Some("One".into()), None);
        catalogs.define("two", Some("Two".into()), None);
        catalogs
            .apply(Spec::Only(vec!["one".into(), "two".into()]))
            .unwrap();
        assert_eq!(members(&l, &catalogs).len(), 1, "membership is a set");
    }
}
