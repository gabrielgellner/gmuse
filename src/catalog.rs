//! Catalogs: several bodies of music, one or more active at a time.
//!
//! A catalog is a **membership rule**, not a second library. DESIGN §4 already
//! makes `Shelf` a projection over a subset of the `Scan`, so confining that
//! subset confines browsing, shuffle and `aaa_mode` with it — `next()` needs no
//! knowledge of catalogs at all.
//!
//! What catalogs deliberately do **not** touch: playlists and the queue hold
//! `TrackId`s straight into the `Scan`, so they cross catalog boundaries
//! freely. That is the point of a playlist.

use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::{Scan, TrackId};

/// The name used when tracks are scanned without any catalog claiming them —
/// `--library` on the command line, or a config with no `[catalogs]` at all.
pub const DEFAULT: &str = "default";

/// One named body of music.
///
/// Membership is `roots` **and** `where`, either of which may be absent. §7
/// settled that a catalog is a named, persistent filter — and it is, but the
/// two keys are kept as two fields rather than desugaring `roots` into a path
/// expression. Desugaring would replace `Path::starts_with`, which compares
/// components, with string globbing, and smart-case would quietly make a
/// lowercase root case-insensitive. Two optional keys describing one rule is
/// not the "two mechanisms for one idea" that warning was about.
#[derive(Debug, Clone)]
pub struct Catalog {
    pub name: String,
    /// A track belongs when its path is under one of these. Empty means the
    /// rule does not look at paths at all.
    pub roots: Vec<PathBuf>,
    /// An expression the track must also match (§7).
    ///
    /// A `where`-only catalog adds nothing to *scan* — it narrows what has
    /// already been indexed, so the files still have to be reachable from
    /// `library` or from some catalog's roots.
    pub filter: Option<crate::filter::Filter>,
}

impl Catalog {
    /// Whether a track belongs. Both halves must hold, and an absent half is
    /// simply not a constraint — so `roots` alone, `where` alone, and both
    /// together all read the same way.
    fn contains(&self, track: &crate::store::Track, env: crate::filter::Env<'_>) -> bool {
        let by_path = self.roots.is_empty() || self.roots.iter().any(|r| under(&track.path, r));
        let by_rule = match &self.filter {
            Some(f) => f.compile(env)(track),
            None => true,
        };
        by_path && by_rule
    }

    /// What the config said, for view 6.
    pub fn rule_label(&self) -> String {
        let mut parts = Vec::new();
        if !self.roots.is_empty() {
            parts.push(
                self.roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            );
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

    /// Add a catalog, merging roots if the name already exists.
    pub fn define(
        &mut self,
        name: &str,
        roots: Vec<PathBuf>,
        filter: Option<crate::filter::Filter>,
    ) {
        if let Some(existing) = self.defined.iter_mut().find(|c| c.name == name) {
            for root in roots {
                if !existing.roots.contains(&root) {
                    existing.roots.push(root);
                }
            }
            if filter.is_some() {
                existing.filter = filter;
            }
        } else {
            self.defined.push(Catalog {
                name: name.to_string(),
                roots,
                filter,
            });
        }
    }

    /// Every root across every catalog.
    ///
    /// Scanning covers *all* of them regardless of what is active, so switching
    /// catalogs is instant rather than a rescan.
    pub fn all_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for catalog in &self.defined {
            for root in &catalog.roots {
                if !roots.contains(root) {
                    roots.push(root.clone());
                }
            }
        }
        roots
    }

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

    /// The tracks the library should be built from.
    ///
    /// With nothing defined, or everything active, this is the whole store —
    /// so a user who never configures a catalog sees no change at all.
    pub fn members(&self, store: &Scan, env: crate::filter::Env<'_>) -> HashSet<TrackId> {
        let active: Vec<&Catalog> = match &self.active {
            Active::All => self.defined.iter().collect(),
            Active::Only(names) => self
                .defined
                .iter()
                .filter(|c| names.contains(&c.name))
                .collect(),
        };

        if self.defined.is_empty() || matches!(self.active, Active::All) {
            return store.iter().map(|(id, _)| id).collect();
        }

        store
            .iter()
            .filter(|(_, track)| active.iter().any(|c| c.contains(track, env)))
            .map(|(id, _)| id)
            .collect()
    }
}

/// Whether `path` is inside `root`.
///
/// A plain `starts_with` on strings would match `/music/rock` against
/// `/music/rockabilly`; `Path::starts_with` compares whole components.
fn under(path: &Path, root: &Path) -> bool {
    path.starts_with(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Track;

    fn store_with(paths: &[&str]) -> Scan {
        let mut s = Scan::new();
        for p in paths {
            s.upsert(crate::store::heard(Track {
                path: PathBuf::from(p),
                ..Default::default()
            }));
        }
        s
    }

    fn fixture() -> (Scan, Catalogs) {
        let store = store_with(&[
            "/m/general/a.mp3",
            "/m/general/b.mp3",
            "/m/electronic/c.mp3",
            "/m/classical/d.mp3",
            "/m/loose.mp3",
        ]);
        let mut catalogs = Catalogs::default();
        catalogs.define("general", vec![PathBuf::from("/m/general")], None);
        catalogs.define("electronic", vec![PathBuf::from("/m/electronic")], None);
        catalogs.define("classical", vec![PathBuf::from("/m/classical")], None);
        (store, catalogs)
    }

    fn paths(store: &Scan, ids: &HashSet<TrackId>) -> BTreeSet<String> {
        ids.iter()
            .map(|id| store.get(*id).unwrap().path.display().to_string())
            .collect()
    }

    #[test]
    fn no_catalogs_means_every_track() {
        let store = store_with(&["/m/a.mp3", "/m/b.mp3"]);
        let catalogs = Catalogs::default();
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            2
        );
        // And nothing to say about it in the status line.
        assert_eq!(catalogs.label(), None);
    }

    #[test]
    fn all_includes_tracks_no_catalog_claims() {
        let (store, catalogs) = fixture();
        // "/m/loose.mp3" is under no catalog root, but `all` must still show
        // it, or scanning a stray folder would make music vanish.
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            5
        );
    }

    #[test]
    fn only_one_catalog_narrows_the_library() {
        let (store, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        assert_eq!(
            paths(
                &store,
                &catalogs.members(&store, crate::filter::Env::default())
            ),
            BTreeSet::from([
                "/m/general/a.mp3".to_string(),
                "/m/general/b.mp3".to_string()
            ])
        );
        assert_eq!(catalogs.label().as_deref(), Some("general"));
    }

    #[test]
    fn a_subset_is_the_union() {
        let (store, mut catalogs) = fixture();
        catalogs
            .apply(Spec::Only(vec!["general".into(), "classical".into()]))
            .unwrap();
        let members = catalogs.members(&store, crate::filter::Env::default());
        assert_eq!(members.len(), 3);
        // Definition order, not alphabetical: the label reads the way the
        // config was written.
        assert_eq!(catalogs.label().as_deref(), Some("general+classical"));
    }

    #[test]
    fn adjusting_from_all_starts_from_everything() {
        let (store, mut catalogs) = fixture();
        // `:catalog -classical` with everything active means "all but that".
        catalogs
            .apply(Spec::Adjust(vec![(false, "classical".into())]))
            .unwrap();
        let members = catalogs.members(&store, crate::filter::Env::default());
        assert!(!paths(&store, &members).contains("/m/classical/d.mp3"));
        assert_eq!(members.len(), 3, "general (2) + electronic (1)");
    }

    #[test]
    fn adjusting_adds_and_removes() {
        let (store, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        catalogs
            .apply(Spec::Adjust(vec![(true, "classical".into())]))
            .unwrap();
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            3
        );
        catalogs
            .apply(Spec::Adjust(vec![(false, "general".into())]))
            .unwrap();
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            1
        );
    }

    #[test]
    fn an_unknown_name_is_an_error_and_changes_nothing() {
        let (store, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        let before = catalogs
            .members(&store, crate::filter::Env::default())
            .len();

        let err = catalogs
            .apply(Spec::Only(vec!["general".into(), "nosuch".into()]))
            .unwrap_err();
        assert!(err.contains("nosuch"), "{err}");
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            before,
            "a bad name must not half-apply"
        );
    }

    #[test]
    fn none_active_is_allowed_and_says_so() {
        let (store, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec![])).unwrap();
        assert!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .is_empty()
        );
        assert_eq!(catalogs.label().as_deref(), Some("none"));
    }

    /// A sibling directory whose name merely starts the same must not be
    /// swept in — the reason this compares path components, not strings.
    #[test]
    fn root_matching_is_by_component_not_prefix() {
        let store = store_with(&["/m/rock/a.mp3", "/m/rockabilly/b.mp3"]);
        let mut catalogs = Catalogs::default();
        catalogs.define("rock", vec![PathBuf::from("/m/rock")], None);
        catalogs.apply(Spec::Only(vec!["rock".into()])).unwrap();
        assert_eq!(
            paths(
                &store,
                &catalogs.members(&store, crate::filter::Env::default())
            ),
            BTreeSet::from(["/m/rock/a.mp3".to_string()])
        );
    }

    #[test]
    fn defining_the_same_name_twice_merges_roots() {
        let mut catalogs = Catalogs::default();
        catalogs.define("mixed", vec![PathBuf::from("/a")], None);
        catalogs.define(
            "mixed",
            vec![PathBuf::from("/b"), PathBuf::from("/a")],
            None,
        );
        assert_eq!(catalogs.defined().len(), 1);
        assert_eq!(catalogs.defined()[0].roots.len(), 2, "no duplicate roots");
    }

    #[test]
    fn scanning_covers_every_root_regardless_of_what_is_active() {
        let (_, mut catalogs) = fixture();
        catalogs.apply(Spec::Only(vec!["general".into()])).unwrap();
        // Otherwise switching catalogs would need a rescan.
        assert_eq!(catalogs.all_roots().len(), 3);
    }

    #[test]
    fn a_track_in_two_catalogs_appears_once() {
        let store = store_with(&["/m/both/a.mp3"]);
        let mut catalogs = Catalogs::default();
        catalogs.define("one", vec![PathBuf::from("/m/both")], None);
        catalogs.define("two", vec![PathBuf::from("/m")], None);
        catalogs
            .apply(Spec::Only(vec!["one".into(), "two".into()]))
            .unwrap();
        assert_eq!(
            catalogs
                .members(&store, crate::filter::Env::default())
                .len(),
            1,
            "membership is a set"
        );
    }
}
