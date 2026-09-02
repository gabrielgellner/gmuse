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

macro_rules! entities {
    ($($(#[$doc:meta])* $name:ident => $kind:literal),* $(,)?) => {
        $(
            $(#[$doc])*
            #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
            pub struct $name;

            impl Entity for $name {
                const KIND: &'static str = $kind;
            }
        )*
    };
}

entities! {
    /// A composition, independent of any performance of it.
    Work => "work",
    /// One performance. **The grain** — what plays, and what history hangs off.
    Recording => "recording",
    /// The album across its editions.
    ReleaseGroup => "release-group",
    /// One edition, with its catalogue number.
    Release => "release",
    /// A disc or a side.
    Medium => "medium",
    /// A recording's slot on a medium.
    Track => "track",
    /// An **encoding** — not a copy. Its id derives from its audio.
    Distribution => "distribution",
    /// A copy of an encoding, on one device.
    Holding => "holding",
    /// A machine that holds copies.
    Device => "device",
    /// A person or an ensemble.
    Artist => "artist",
    /// An imprint.
    Label => "label",
    /// A concept in the genre vocabulary.
    Genre => "genre",
    /// One grouping of genre concepts — a list may carry several.
    Hierarchy => "hierarchy",
    /// What an artist did on something, as a codelist entry.
    Role => "role",
    /// A curated set, ordered or not.
    Collection => "collection",
    /// An artist on a work, recording, release group or release.
    Credit => "credit",
    /// A recording's place in a mix, with its entry time.
    MixEntry => "mix-entry",
    /// One recording's membership of one collection.
    CollectionEntry => "collection-entry",
    /// One genre assigned to one subject, with why and how confidently.
    GenreAssignment => "genre-assignment",
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    /// A reused `KIND` would silently merge two kinds' derived ids, which is
    /// the failure this test exists to make loud.
    #[test]
    fn every_kind_name_is_distinct() {
        let kinds = [
            Work::KIND,
            Recording::KIND,
            ReleaseGroup::KIND,
            Release::KIND,
            Medium::KIND,
            Track::KIND,
            Distribution::KIND,
            Holding::KIND,
            Device::KIND,
            Artist::KIND,
            Label::KIND,
            Genre::KIND,
            Hierarchy::KIND,
            Role::KIND,
            Collection::KIND,
            Credit::KIND,
            MixEntry::KIND,
            CollectionEntry::KIND,
            GenreAssignment::KIND,
        ];
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
