//! What the model is made of, and the name each kind mints under.
//!
//! These are the entities of `domain-model.md` §2. They are placeholders: the
//! fields arrive with #72–#76, and nothing here depends on their being empty —
//! `Id<T>` only ever uses `T` as a tag.
//!
//! What they carry *now* is [`Entity::KIND`], which is load-bearing before a
//! single field exists.

/// A kind of thing the catalogue holds.
///
/// ## Why a name, and why it must never change
///
/// Derived ids are minted as `UUIDv5(NAMESPACE, seed)` (#66), and without a
/// per-kind prefix two different things minted from the same seed bytes would
/// land on the same UUID. The Rust types would still not unify — that is what
/// the phantom is for — but the *values* would be identical on disk, in an
/// export, and in every log, which is where identity actually has to hold.
///
/// So the seed is `KIND` then `0x1f` then the bytes. `KIND` is part of every
/// derived id ever written, exactly as `NAMESPACE` is: changing one re-mints
/// every entity of that kind and nothing else, which is the more insidious
/// failure of the two because it looks local.
pub trait Entity {
    /// Stable, lowercase, never reused for a different kind.
    const KIND: &'static str;
}

macro_rules! kinds {
    ($($(#[$doc:meta])* $name:ident => $kind:literal),* $(,)?) => {
        $(
            impl Entity for $name {
                const KIND: &'static str = $kind;
            }
        )*

        #[allow(dead_code, reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the kind list the reachability ratchet reads instead of holding its own copy")]
        /// Every kind, from the one place they are declared.
        ///
        /// **Emitted by the macro rather than typed out again.** Two tests ask
        /// questions about the whole set — that the names are distinct, and
        /// that each is reachable from a fact — and a hand-copied second list
        /// silently stops asking about anything added after it was written.
        /// Adding `Qualifier` proved that: the reachability ratchet held its
        /// own copy and passed a kind it had never heard of.
        pub const ALL_KINDS: &[&str] = &[$($kind),*];
    };
}

/// Declare the kinds that are still only tags.
///
/// Their fields arrive with their own issue; until then they exist so that
/// `Id<T>` can name them. Giving one fields later does not disturb anything
/// already minted, because an id only ever uses `T` as a tag.
macro_rules! tags {
    ($($(#[$doc:meta])* $name:ident),* $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub struct $name;
        )*
    };
}

tags! {
    #[expect(dead_code, reason = "a tag type for an entity the domain model proposes and nothing mints yet — fields with #75")]
    /// A copy of an encoding, on one device. Fields with #75.
    Holding,
    /// A machine that holds copies. Fields with #75.
    Device,
    /// A person or an ensemble. Fields with #73.
    Artist,
    /// An imprint. Fields with #73.
    Label,
    /// A concept in the genre vocabulary. Fields with #74.
    Genre,
    /// A concept in the **version** vocabulary — instrumental, live, acappella.
    ///
    /// A separate scheme from `Genre` rather than a corner of it, because the
    /// two answer different questions and are curated by different evidence: a
    /// genre says what kind of music this is, a qualifier says which version of
    /// it you are holding. Sharing a list would make `genre = instrumental`
    /// expressible, and it is not a genre.
    Qualifier,
    /// One grouping of genre concepts. Fields with #74.
    Hierarchy,
    /// What an artist did on something. Fields with #73.
    Role,
    #[expect(dead_code, reason = "proposed by the domain model, minted by nothing — fields with #76")]
    /// A curated set, ordered or not. Fields with #76.
    Collection,
    #[expect(dead_code, reason = "proposed by the domain model, minted by nothing — fields with #73")]
    /// An artist on a work, recording, release group or release. Fields with #73.
    Credit,
    #[expect(dead_code, reason = "proposed by the domain model, minted by nothing — fields with #76")]
    /// One recording's membership of one collection. Fields with #76.
    CollectionEntry,
    #[expect(dead_code, reason = "proposed by the domain model, minted by nothing — fields with #74")]
    /// One genre assigned to one subject. Fields with #74.
    GenreAssignment,
}

kinds! {
    Work => "work",
    Recording => "recording",
    ReleaseGroup => "release-group",
    Release => "release",
    Medium => "medium",
    Track => "track",
    Encoding => "encoding",
    Holding => "holding",
    Device => "device",
    Artist => "artist",
    Label => "label",
    Genre => "genre",
    Qualifier => "qualifier",
    Hierarchy => "hierarchy",
    Role => "role",
    Collection => "collection",
    Credit => "credit",
    MixEntry => "mix-entry",
    CollectionEntry => "collection-entry",
    GenreAssignment => "genre-assignment",
}

pub use crate::model::encoding::Encoding;
pub use crate::model::spine::{Medium, MixEntry, Recording, Release, ReleaseGroup, Track, Work};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A reused `KIND` would silently merge two kinds' derived ids, which is
    /// the failure this test exists to make loud.
    #[test]
    fn every_kind_name_is_distinct() {
        let kinds = ALL_KINDS;
        let unique: HashSet<_> = kinds.iter().collect();
        assert_eq!(unique.len(), kinds.len(), "two kinds share a name");
        assert!(
            kinds
                .iter()
                .all(|k| !k.is_empty() && k.chars().all(|c| c.is_ascii_lowercase() || c == '-')),
            "a kind name must be stable and boring: lowercase and dashes"
        );
    }
}
