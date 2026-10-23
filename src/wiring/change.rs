//! **What an append tells the views** — #218, #222, §8.7, moved here by #257.
//!
//! It lived in `gmuse-engine`, which is why that crate could name `Touched` and `Changed`.
//! The translation needs a library type and a protocol type at once, and the root crate is
//! the only place both are legitimately visible — see the module header.
//!
//! The librarian already classifies how far a change reaches: `Changed` is
//! `Nothing`, `These`, `Wide` or `Rebuild`, folded by `Changed::of` from each
//! fact's own `blast()`. This renders that onto the wire, and does not work it
//! out a second time.

use gmuse_library::facts::Touched;
use gmuse_library::librarian::Changed;
use gmuse_proto::{Change, Event, Topic};

/// The event a published answer should produce.
///
/// **A rendering of `librarian::Changed`, not a second derivation of it.**
///
/// The first version walked the records and mapped `Fact::blast()` itself,
/// which is exactly what `Changed::of` does forty lines away in the library —
/// and `Changed::and` is the same lattice join as `Change::join`. Two
/// implementations of one coalescing rule is the drift
/// `every_fact_names_its_subject` exists to prevent, arriving one layer up:
/// both compile, both look right, and the day a fact's blast radius changes
/// only one of them is told.
///
/// So the library owns the lattice and this owns the wire. #218 said as much —
/// "a serialisation of something already proven rather than a new invention" —
/// and it was written as a new invention anyway.
pub fn change_for(changed: &Changed, version: u64) -> Option<Event> {
    let change = match changed {
        Changed::Nothing => return None,
        Changed::Rebuild => Change::rebuild(version),
        Changed::Wide => Change::wide(version),
        Changed::These(touched) => {
            let ids: Vec<String> = touched.iter().map(name_of).collect();
            // **An empty set is a wide, not an empty refresh.** A view told
            // `scope: "ids"` with no ids would conclude nothing it displays
            // changed, which is the one thing certainly false about a change
            // that was just published.
            if ids.is_empty() {
                Change::wide(version)
            } else {
                Change::ids(version, ids)
            }
        }
    };
    Some(Event::new(
        Topic::Library,
        serde_json::to_value(change).ok()?,
    ))
}

/// **`kind:uuid`**, which is §8.7's `"rec:8f3a…"` shape generalised.
///
/// A view matching rows by id needs the kind as well as the number: two
/// entities of different kinds can never share a derived id (`Entity::KIND`
/// seeds every one), but a view holding a heterogeneous list still has to know
/// which of its collections to look in.
fn name_of(touched: &Touched) -> String {
    match touched {
        Touched::Artist(id) => format!("artist:{id}"),
        Touched::Recording(id) => format!("recording:{id}"),
        Touched::Encoding(id) => format!("encoding:{id}"),
        Touched::Collection(id) => format!("collection:{id}"),
        Touched::View(id) => format!("view:{id}"),
        Touched::Rule(id) => format!("rule:{id}"),
        Touched::Facet(id) => format!("facet:{id}"),
        Touched::Concept(id) => format!("concept:{id}"),
        Touched::Hierarchy(id) => format!("hierarchy:{id}"),
        Touched::Work(id) => format!("work:{id}"),
        Touched::Device(id) => format!("device:{id}"),
        Touched::ReleaseGroup(id) => format!("release-group:{id}"),
        Touched::Release(id) => format!("release:{id}"),
        Touched::Medium(id) => format!("medium:{id}"),
        Touched::Track(id) => format!("track:{id}"),
        Touched::Role(id) => format!("role:{id}"),
        Touched::Label(id) => format!("label:{id}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gmuse_ids::id::Id;
    use gmuse_proto::Scope;

    fn change(changed: &Changed, version: u64) -> gmuse_proto::Change {
        let event = change_for(changed, version).expect("an event");
        serde_json::from_value(event.body).expect("a change")
    }

    #[test]
    fn nothing_arrived_so_nothing_is_said() {
        assert!(change_for(&Changed::Nothing, 1).is_none());
    }

    /// §8.7's Done-when shape: a narrow change names exactly what moved.
    #[test]
    fn a_narrow_change_names_the_rows_it_touched() {
        let recording = Id::derived(b"one");
        let changed = Changed::These([Touched::Recording(recording)].into_iter().collect());
        let change = change(&changed, 42);
        assert_eq!(change.scope, Scope::Ids);
        assert_eq!(change.version, 42);
        assert_eq!(change.ids, vec![format!("recording:{recording}")]);
    }

    /// **The id carries its kind.** A view holding a heterogeneous list has to
    /// know which collection to look in.
    #[test]
    fn an_id_says_what_kind_of_thing_it_names() {
        let changed = Changed::These(
            [
                Touched::Recording(Id::derived(b"r")),
                Touched::Artist(Id::derived(b"a")),
            ]
            .into_iter()
            .collect(),
        );
        let ids = change(&changed, 1).ids;
        assert!(ids.iter().any(|i| i.starts_with("recording:")), "{ids:?}");
        assert!(ids.iter().any(|i| i.starts_with("artist:")), "{ids:?}");
    }

    #[test]
    fn a_wide_change_asks_for_a_re_query() {
        assert_eq!(change(&Changed::Wide, 3).scope, Scope::Wide);
        assert_eq!(change(&Changed::Rebuild, 3).scope, Scope::Rebuild);
    }

    /// An empty `These` is a wide, not an empty refresh: a view told nothing
    /// changed would be told the one thing that is certainly false.
    #[test]
    fn an_empty_set_of_touches_is_a_wide() {
        assert_eq!(
            change(&Changed::These(Default::default()), 1).scope,
            Scope::Wide
        );
    }

    /// **The two lattices agree, which is the property the refactor buys.**
    /// `Changed::and` and `Change::join` are the same join; if they ever
    /// disagree, one of them has been changed without the other.
    #[test]
    fn the_librarys_lattice_and_the_wires_lattice_are_the_same_join() {
        let a = Changed::These(
            [Touched::Recording(Id::derived(b"a"))]
                .into_iter()
                .collect(),
        );
        let b = Changed::These(
            [Touched::Recording(Id::derived(b"b"))]
                .into_iter()
                .collect(),
        );

        let folded_in_library = change(&a.clone().and(b.clone()), 2);
        let folded_on_wire = change(&a, 1).join(&change(&b, 2));

        assert_eq!(folded_in_library.scope, folded_on_wire.scope);
        assert_eq!(folded_in_library.ids, folded_on_wire.ids);
        assert_eq!(folded_in_library.version, folded_on_wire.version);
    }
}
