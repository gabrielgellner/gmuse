//! **What an append tells the views** — #218, §8.7.
//!
//! The log already classifies how far a fact reaches: `Fact::blast()` is
//! Narrow, Wide or Rebuild. This turns a batch of appended facts into the one
//! `library` event a view needs, which is the same classification plus the ids
//! where they are derivable.

use gmuse_library::facts::{Blast, Fact, Record, Touched};
use gmuse_proto::{Change, Event, Topic};

/// The event a batch of appended records should produce.
///
/// **One event for the batch, not one per fact.** An apply lands thousands of
/// facts; a view told about each would spend the whole operation re-querying,
/// and §8.7's lattice says what the answer is instead — the join of all of
/// them. Folding here rather than letting the outbox do it also means a client
/// that is keeping up gets the same answer as one that is behind.
pub fn change_for(records: &[Record], version: u64) -> Option<Event> {
    let mut folded: Option<Change> = None;
    for record in records {
        let one = match record.fact.blast() {
            Blast::Narrow => {
                let ids = subjects(&record.fact);
                // **An empty narrow is a wide**, not an empty refresh. A view
                // told `scope: "ids"` with no ids would conclude that nothing
                // it displays changed, which is the one thing that is certainly
                // false about a fact that was just appended.
                if ids.is_empty() {
                    Change::wide(version)
                } else {
                    Change::ids(version, ids)
                }
            }
            Blast::Wide => Change::wide(version),
            Blast::Rebuild => Change::rebuild(version),
        };
        folded = Some(match folded {
            Some(so_far) => so_far.join(&one),
            None => one,
        });
    }
    let change = folded?;
    Some(Event::new(
        Topic::Library,
        serde_json::to_value(change).ok()?,
    ))
}

/// The ids a narrow fact names, as the fact itself reports them.
///
/// **`Fact::touches()`, not a second table and not a heuristic.** It is
/// exhaustive on purpose — "a new fact does not compile until somebody says
/// what it reaches, which is the only thing that stops this drifting quietly
/// out of step with the fold" — and that is exactly the property a blast-radius
/// event needs.
///
/// The first version of this walked the fact's serialisation collecting
/// UUID-shaped strings, on the reasoning that a narrow fact's ids *are* its
/// blast radius. That is true and it was still the wrong way to get them:
/// `touches()` already exists two hundred lines from `blast()`, the two are
/// kept in step by the same rule, and a heuristic beside them would have been a
/// third opinion that compiles when a variant is added and is silently wrong.
fn subjects(fact: &Fact) -> Vec<String> {
    fact.touches().iter().map(name_of).collect()
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
    use gmuse_library::model::clock::Stamp;
    use gmuse_library::model::id::Id;
    use gmuse_proto::Scope;

    fn record(fact: Fact) -> Record {
        Record {
            // Any stamp: this maps facts to a blast radius and never reads one.
            stamp: Stamp {
                wall: 0,
                counter: 0,
                device: Id::derived(b"test-device"),
            },
            fact,
        }
    }

    fn change(records: &[Record], version: u64) -> gmuse_proto::Change {
        let event = change_for(records, version).expect("an event");
        serde_json::from_value(event.body).expect("a change")
    }

    #[test]
    fn an_empty_batch_says_nothing() {
        assert!(change_for(&[], 1).is_none());
    }

    /// §8.7's Done-when shape: a narrow fact names exactly what changed.
    #[test]
    fn a_narrow_fact_names_the_row_it_touched() {
        let recording = Id::derived(b"one");
        let batch = [record(Fact::RecordingRated {
            recording,
            stars: Some(4),
        })];
        let change = change(&batch, 42);
        assert_eq!(change.scope, Scope::Ids);
        assert_eq!(change.version, 42);
        assert_eq!(change.ids, vec![format!("recording:{recording}")]);
    }

    /// **One event for the batch, not one per fact.** An apply lands thousands;
    /// a view told about each would spend the whole operation re-querying.
    #[test]
    fn a_batch_of_narrow_facts_is_one_event_listing_all_of_them() {
        let (a, b) = (Id::derived(b"a"), Id::derived(b"b"));
        let batch = [
            record(Fact::RecordingRated {
                recording: a,
                stars: Some(4),
            }),
            record(Fact::RecordingRated {
                recording: b,
                stars: Some(5),
            }),
        ];
        let change = change(&batch, 7);
        assert_eq!(change.scope, Scope::Ids);
        assert_eq!(change.ids.len(), 2, "both, not the last: {:?}", change.ids);
    }

    /// The same row twice is one entry — a view refreshing it twice is waste,
    /// and the lattice already says so.
    #[test]
    fn the_same_row_touched_twice_is_listed_once() {
        let a = Id::derived(b"a");
        let batch = [
            record(Fact::RecordingRated {
                recording: a,
                stars: Some(4),
            }),
            record(Fact::RecordingRated {
                recording: a,
                stars: Some(5),
            }),
        ];
        assert_eq!(change(&batch, 1).ids.len(), 1);
    }

    /// **The id carries its kind.** A view holding a heterogeneous list has to
    /// know which collection to look in.
    #[test]
    fn an_id_says_what_kind_of_thing_it_names() {
        let batch = [record(Fact::RecordingRated {
            recording: Id::derived(b"one"),
            stars: Some(4),
        })];
        assert!(
            change(&batch, 1).ids[0].starts_with("recording:"),
            "got {:?}",
            change(&batch, 1).ids
        );
    }
}
