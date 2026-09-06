//! One addressable thing in the model — issue #124, `info-panel.md` §3.
//!
//! The info panel is always focused on exactly one entity. This is the enum of
//! what that can be: what a link points at, what the navigation stack pushes,
//! and what `:info` resolves to.
//!
//! ## Why a fourth name, when three exist
//!
//! [`crate::model::proposal::Subject`] is what a *proposal* is about — an
//! encoding, a recording, or the vocabulary. [`crate::model::facet::Described`]
//! is the two things a *concept* can be asserted of. Neither means "anything
//! the panel can show", and widening either would make it mean two things.
//!
//! Three near-synonyms in one codebase is how a reader stops trusting any of
//! them. `Focus` says what it is: the thing the panel is focused on.
//!
//! ## What it does not carry
//!
//! **A label.** Naming a recording needs the spine, naming a concept needs the
//! vocabulary, and a type that had to be handed a library before it could say
//! what it was would be a poor thing to put on a stack. `About` answers that
//! (#125), and this stays a name for a place.

use crate::model::entity::{
    Artist, Collection, Concept, Encoding, Entity, Recording, Release, ReleaseGroup, Work,
};
use crate::model::id::Id;

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the info panel is #125 onward, and this is the type it holds"
)]
/// A thing the info panel can be focused on.
///
/// **Eight kinds, and every one has an id of its own in the fold.** The
/// design doc listed `Holding` too, and it is not here: a holding is
/// `(device, encoding) -> locator`, with no id ever minted for it, and what a
/// holding page would show — where a copy is, on which machine — is what the
/// *encoding* page shows a row of. A variant nothing can address is a variant
/// nothing can test.
///
/// `Facet`, `Hierarchy`, `Role`, `Label`, `Device` and `View` are absent for
/// the same reason in reverse: nothing links to them yet. They arrive with the
/// section that reaches them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Focus {
    Recording(Id<Recording>),
    /// A copy — the bytes. A row under the cursor is one of these.
    Encoding(Id<Encoding>),
    ReleaseGroup(Id<ReleaseGroup>),
    Release(Id<Release>),
    Artist(Id<Artist>),
    /// A concept on some axis. **The id already carries the axis**, because it
    /// is minted from the facet key and the label (#117), so nothing here has
    /// to hold the facet beside it.
    Concept(Id<Concept>),
    Collection(Id<Collection>),
    Work(Id<Work>),
}

#[allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — the info panel is #125 onward"
)]
impl Focus {
    /// The word for what this is, as the panel's corner shows it.
    ///
    /// **[`Entity::KIND`], never a second list.** That constant is part of
    /// every derived id ever written, so a spelling typed out again here could
    /// drift from the one the ids were minted under and nothing would fail
    /// until an id did not resolve.
    pub fn kind(self) -> &'static str {
        match self {
            Focus::Recording(_) => Recording::KIND,
            Focus::Encoding(_) => Encoding::KIND,
            Focus::ReleaseGroup(_) => ReleaseGroup::KIND,
            Focus::Release(_) => Release::KIND,
            Focus::Artist(_) => Artist::KIND,
            Focus::Concept(_) => Concept::KIND,
            Focus::Collection(_) => Collection::KIND,
            Focus::Work(_) => Work::KIND,
        }
    }

    /// The id, whatever kind it is of.
    ///
    /// The phantom is what keeps the kinds apart in the type system; this is
    /// for writing one down and for comparing two of the same kind.
    pub fn uuid(self) -> uuid::Uuid {
        match self {
            Focus::Recording(id) => id.as_uuid(),
            Focus::Encoding(id) => id.as_uuid(),
            Focus::ReleaseGroup(id) => id.as_uuid(),
            Focus::Release(id) => id.as_uuid(),
            Focus::Artist(id) => id.as_uuid(),
            Focus::Concept(id) => id.as_uuid(),
            Focus::Collection(id) => id.as_uuid(),
            Focus::Work(id) => id.as_uuid(),
        }
    }

    /// `recording:0192…` — exact, and needing no library to produce.
    ///
    /// **This is not how a person names one.** `:info artist="Bodzin"` is a
    /// name to be resolved against the library, which is #129's job and needs
    /// the catalogue. What this is for is the forms that must not depend on
    /// what the library happens to hold: a remembered focus, an audit line, a
    /// test that says exactly which thing it meant.
    pub fn written(self) -> String {
        format!("{}:{}", self.kind(), self.uuid())
    }

    /// Read one back. The inverse of [`Self::written`].
    pub fn parse(text: &str) -> Option<Self> {
        let (kind, id) = text.trim().split_once(':')?;
        let uuid: uuid::Uuid = id.trim().parse().ok()?;
        Some(match kind.trim() {
            k if k == Recording::KIND => Focus::Recording(Id::from_uuid(uuid)),
            k if k == Encoding::KIND => Focus::Encoding(Id::from_uuid(uuid)),
            k if k == ReleaseGroup::KIND => Focus::ReleaseGroup(Id::from_uuid(uuid)),
            k if k == Release::KIND => Focus::Release(Id::from_uuid(uuid)),
            k if k == Artist::KIND => Focus::Artist(Id::from_uuid(uuid)),
            k if k == Concept::KIND => Focus::Concept(Id::from_uuid(uuid)),
            k if k == Collection::KIND => Focus::Collection(Id::from_uuid(uuid)),
            k if k == Work::KIND => Focus::Work(Id::from_uuid(uuid)),
            _ => return None,
        })
    }
}

impl std::fmt::Display for Focus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.written())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One of every variant, so a kind added without a test is a kind the
    /// tests below do not silently skip.
    fn all() -> Vec<Focus> {
        let u = |n: u128| uuid::Uuid::from_u128(n);
        vec![
            Focus::Recording(Id::from_uuid(u(1))),
            Focus::Encoding(Id::from_uuid(u(2))),
            Focus::ReleaseGroup(Id::from_uuid(u(3))),
            Focus::Release(Id::from_uuid(u(4))),
            Focus::Artist(Id::from_uuid(u(5))),
            Focus::Concept(Id::from_uuid(u(6))),
            Focus::Collection(Id::from_uuid(u(7))),
            Focus::Work(Id::from_uuid(u(8))),
        ]
    }

    /// **Every focus round-trips through its written form**, which is what
    /// lets one be remembered, audited or named exactly.
    #[test]
    fn a_focus_reads_back_as_what_was_written() {
        for focus in all() {
            let text = focus.written();
            assert_eq!(Focus::parse(&text), Some(focus), "{text} did not read back");
        }
    }

    /// **The kinds are the model's own**, not a second spelling of them.
    ///
    /// `Entity::KIND` is part of every derived id ever written. A word typed
    /// out again here could drift from the one the ids were minted under, and
    /// nothing would fail until an id did not resolve — which is the failure
    /// `ALL_KINDS` exists to make impossible.
    #[test]
    fn every_kind_is_one_the_model_declares() {
        for focus in all() {
            assert!(
                crate::model::entity::ALL_KINDS.contains(&focus.kind()),
                "{} is not a kind the model declares",
                focus.kind()
            );
        }
    }

    /// Two kinds sharing a spelling would make `parse` answer with whichever
    /// arm came first, silently.
    #[test]
    fn no_two_kinds_are_written_the_same_way() {
        let mut seen = std::collections::BTreeSet::new();
        for focus in all() {
            assert!(seen.insert(focus.kind()), "{} appears twice", focus.kind());
        }
        assert_eq!(seen.len(), all().len(), "a variant was left out of `all`");
    }

    /// **The kind is part of the identity**, not decoration. Two entities of
    /// different kinds may hold the same uuid — nothing stops it, because the
    /// phantom keeps them apart in the type system and not on disk — and a
    /// focus that ignored the kind would confuse them.
    #[test]
    fn one_uuid_under_two_kinds_is_two_places() {
        let u = uuid::Uuid::from_u128(1);
        let recording = Focus::Recording(Id::from_uuid(u));
        let work = Focus::Work(Id::from_uuid(u));
        assert_ne!(recording, work);
        assert_ne!(recording.written(), work.written());
        assert_eq!(Focus::parse(&work.written()), Some(work));
    }

    /// Nonsense is refused rather than guessed at — the rule the `:` line
    /// follows everywhere else (#119).
    #[test]
    fn a_kind_nothing_declares_is_not_a_focus() {
        for bad in [
            "",
            "recording",
            "recording:",
            ":0000",
            "sandwich:00000000-0000-0000-0000-000000000001",
            "recording:not-a-uuid",
        ] {
            assert_eq!(Focus::parse(bad), None, "{bad:?} parsed");
        }
    }
}
