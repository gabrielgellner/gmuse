//! The minting constructors whose kinds live elsewhere — #213.
//!
//! **Here because of the orphan rule, and that is the honest reason.** These
//! are inherent `impl`s on `Id<T>`, so they can only be written in the crate
//! that defines `Id`. They were spread across `facet.rs` and `reference.rs`
//! next to the vocabularies they mint for, which read better and stopped
//! compiling the moment `Id` moved.
//!
//! What makes that acceptable rather than a workaround: every one of them *is*
//! the UUIDv5 derivation, which §4.3 puts in this crate. A derived id's value
//! is part of the identity contract — change one of these and every entity of
//! that kind is renamed, silently and everywhere — so they belong next to
//! `Id::derived` and the `KIND` strings they seed with.

use crate::entity::{Concept, Facet, Label, Role};
use crate::id::Id;

impl Id<Facet> {
    /// The id a facet's key always mints.
    pub fn of_key(key: &str) -> Self {
        Id::derived(key.trim().to_ascii_lowercase().as_bytes())
    }
}

impl Id<Facet> {
    /// The array this axis's edges live in, unless somebody defines another.
    ///
    /// A facet may carry several hierarchies — a browse one and a filter one
    /// over the same list, as SDMX does — so this is *a* hierarchy rather than
    /// *the* hierarchy. It exists because the seeded vocabularies need
    /// somewhere to put their edges without inventing a name each.
    pub fn array(key: &str) -> Id<crate::entity::Hierarchy> {
        Id::derived(key.trim().to_ascii_lowercase().as_bytes())
    }
}

impl Id<Concept> {
    /// The id a label always mints **on one axis**.
    ///
    /// **The facet is the domain separator**, which is why this does not go
    /// through [`Id::derived`] and its `Entity::KIND`. A concept's namespace
    /// is its facet — that is what the type is *for* — so seeding with
    /// `Concept::KIND` would put every axis back in one namespace and let
    /// `ambient` the style collide with `ambient` the function.
    ///
    /// Two properties fall out. Cross-facet collision is impossible. And
    /// because the genre facet's key is the string `Genre::KIND` used to be,
    /// **every genre id ever minted keeps its exact value** — pinned by a test
    /// below, for the reason `NAMESPACE` is pinned: a change here re-mints a
    /// library silently.
    ///
    /// Normalised like every other named concept, so two devices cataloguing
    /// `Techno` and `techno` independently arrive at one id with no registry
    /// between them.
    pub fn in_facet(facet_key: &str, label: &str) -> Self {
        let mut seed = Vec::new();
        seed.extend_from_slice(facet_key.trim().to_ascii_lowercase().as_bytes());
        seed.push(0x1f); // the separator `Id::derived` uses, same reason
        seed.extend_from_slice(crate::normalise(label).as_bytes());
        Id::from_uuid(uuid::Uuid::new_v5(&crate::id::NAMESPACE, &seed))
    }
}

impl Id<Label> {
    /// The id an imprint's name always mints.
    ///
    /// Through [`crate::normalise`], like every other concept:
    /// two devices reading the same sleeve land on the same imprint without
    /// meeting. 218 distinct labels here, and `Columbia` on one file must be
    /// `Columbia` on the next.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::normalise(name).as_bytes())
    }
}

impl Id<Role> {
    /// The id a role name always mints.
    ///
    /// Through [`crate::normalise`], for the reason concept ids
    /// are: the codelist has to be definable on two machines that never meet.
    /// It replaces `Id::derived(b"performer")` written inline in the
    /// projection, which was one role, minted once, handed to everybody.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::normalise(name).as_bytes())
    }
}
