//! **What changed in the library, and how much a view must do about it** —
//! §8.7.
//!
//! An event that said only "something changed" would force every view to
//! re-query everything on every fact. The log already classifies this —
//! `Fact::blast()` is Narrow, Wide or Rebuild — and that maps directly onto
//! what a view needs to be told.

use serde::{Deserialize, Serialize};

/// How much of a view is invalidated.
///
/// **A lattice, and that is what makes coalescing well-defined.** §7.10 says
/// events coalesce latest-wins per topic, which for this topic would be wrong:
/// two `ids` events are two disjoint statements, and keeping only the newer
/// silently drops rows from the refresh. [`Change::join`] is the operation that
/// makes "latest-wins" mean something here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    /// Refresh exactly these rows. From `Blast::Narrow`.
    Ids,
    /// Re-query whatever is currently displayed. From `Blast::Wide`.
    Wide,
    /// Drop caches and re-query from scratch. From `Blast::Rebuild`.
    Rebuild,
}

/// One `library` event's payload.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    /// The engine's log position.
    ///
    /// **A scalar because the engine is the single appender** (§8.7). Facts
    /// from another device arrive through an import the engine also performs.
    /// It is not a global version across devices: sync identity is a vector
    /// clock and lives in the sync design, not on this wire.
    pub version: u64,
    pub scope: Scope,
    /// Present for `Ids`, and meaningless otherwise — a wide change is
    /// precisely one whose affected set is not derivable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ids: Vec<String>,
}

impl Change {
    pub fn ids(version: u64, ids: impl IntoIterator<Item = String>) -> Self {
        let mut ids: Vec<String> = ids.into_iter().collect();
        ids.sort();
        ids.dedup();
        Self {
            version,
            scope: Scope::Ids,
            ids,
        }
    }

    pub fn wide(version: u64) -> Self {
        Self {
            version,
            scope: Scope::Wide,
            ids: Vec::new(),
        }
    }

    pub fn rebuild(version: u64) -> Self {
        Self {
            version,
            scope: Scope::Rebuild,
            ids: Vec::new(),
        }
    }

    /// Combine two changes into one that is true of both — §8.7's lattice.
    ///
    /// Two `ids` events **union** their lists; `ids` joined with `wide` is
    /// `wide`; anything joined with `rebuild` is `rebuild`. The version is the
    /// later of the two, because a view acting on the result has caught up to
    /// both.
    ///
    /// **This is the whole reason the coalescing policy is safe for this
    /// topic.** Dropping the older event would drop rows that are genuinely
    /// stale in the view, and the view would have no way to know: §7.8's
    /// "events describe state" holds for a `playback` event, which is a
    /// complete statement, and does not hold for a list of what changed unless
    /// the lists are merged.
    pub fn join(&self, other: &Change) -> Change {
        let version = self.version.max(other.version);
        match self.scope.max(other.scope) {
            Scope::Rebuild => Change::rebuild(version),
            Scope::Wide => Change::wide(version),
            Scope::Ids => Change::ids(version, self.ids.iter().chain(&other.ids).cloned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: u64, names: &[&str]) -> Change {
        Change::ids(v, names.iter().map(|s| (*s).to_string()))
    }

    /// **The bug this type exists to prevent.** Latest-wins on two `ids`
    /// events drops the older list, and the rows in it stay stale on screen
    /// with nothing to tell the view otherwise.
    #[test]
    fn two_narrow_changes_union_their_ids() {
        let joined = ids(1, &["rec:a"]).join(&ids(2, &["rec:b"]));
        assert_eq!(joined.scope, Scope::Ids);
        assert_eq!(joined.ids, vec!["rec:a", "rec:b"]);
        assert_eq!(joined.version, 2, "caught up to both");
    }

    #[test]
    fn a_repeated_id_is_not_listed_twice() {
        let joined = ids(1, &["rec:a"]).join(&ids(2, &["rec:a", "rec:b"]));
        assert_eq!(joined.ids, vec!["rec:a", "rec:b"]);
    }

    /// Wide absorbs ids: once a view has to re-query what it displays, knowing
    /// which rows also changed adds nothing.
    #[test]
    fn wide_absorbs_ids_in_either_order() {
        assert_eq!(ids(1, &["rec:a"]).join(&Change::wide(2)).scope, Scope::Wide);
        assert_eq!(Change::wide(1).join(&ids(2, &["rec:a"])).scope, Scope::Wide);
    }

    #[test]
    fn rebuild_absorbs_everything() {
        for other in [ids(1, &["rec:a"]), Change::wide(1), Change::rebuild(1)] {
            assert_eq!(Change::rebuild(2).join(&other).scope, Scope::Rebuild);
            assert_eq!(other.join(&Change::rebuild(2)).scope, Scope::Rebuild);
        }
    }

    /// A lattice join is associative and commutative, which is what lets the
    /// outbox fold events in arrival order without thinking about it.
    #[test]
    fn the_join_does_not_depend_on_the_order_it_is_folded_in() {
        let a = ids(1, &["rec:a"]);
        let b = ids(2, &["rec:b"]);
        let c = Change::wide(3);
        assert_eq!(a.join(&b).join(&c), a.join(&b.join(&c)));
        assert_eq!(a.join(&b), b.join(&a));
    }

    /// A wide change carries no ids, and the field is absent rather than an
    /// empty array — a view checking for `ids` should not find one that means
    /// nothing.
    #[test]
    fn a_wide_change_sends_no_id_list() {
        let line = serde_json::to_string(&Change::wide(7)).unwrap();
        assert_eq!(line, r#"{"version":7,"scope":"wide"}"#);
    }

    #[test]
    fn the_documented_shape_parses() {
        let change: Change = serde_json::from_str(
            r#"{"version":918273,"scope":"ids","ids":["rec:8f3a","rel:12b4"]}"#,
        )
        .unwrap();
        assert_eq!(change.version, 918_273);
        assert_eq!(change.ids.len(), 2);
    }
}
