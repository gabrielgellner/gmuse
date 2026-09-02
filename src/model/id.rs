//! The identity layer — issue #65, `domain-model.md` §3.
//!
//! **Two things are called identity and they are different.** Conflating them
//! is how a history keyed by an index silently reattaches to the wrong track
//! after a rebuild, which `history.rs` already warns about.
//!
//! | | what it is | lifetime |
//! | --- | --- | --- |
//! | [`Id`] | the identity a reference names | durable, travels, persisted |
//! | [`Handle`] | an arena index | process-local, **never persisted** |
//! | [`Ref`] | an id that may carry its resolution | persists as the id alone |
//!
//! `history.rs` is right that "a history keyed by id would silently reattach to
//! different tracks" — but that rejects the *arena index*, not identity. This
//! module keeps both and makes them impossible to confuse: a `Handle` cannot be
//! serialised, so nothing durable can accidentally hold one.
//!
//! ## The portability invariant
//!
//! > A reference points at something at least as portable as itself, never
//! > less.
//!
//! Something local may name something global — a `Holding` names its
//! `Distribution`, a copy naming the encoding it holds. The reverse cannot be
//! written: a `Recording` naming one machine's copy would be inert on every
//! other machine. The type system enforces the sharp end of this, because
//! [`Handle`] has no `Serialize`, so a persisted struct can only hold an
//! [`Id`].
//!
//! ## Why the phantom, and why the hand-written impls
//!
//! `Id<Recording>` and `Id<Release>` do not unify — the type parameter is a
//! tag, at no runtime cost. It also means `#[derive(Clone)]` is wrong here:
//! derive would add a `T: Clone` bound, and `T` is a marker that is never
//! constructed and need not implement anything. Every impl below is written
//! out for that reason.

use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;

use uuid::Uuid;

/// The namespace every derived id is minted under.
///
/// **This constant is load-bearing and must never change.** Every convergent
/// id (#66) is `UUIDv5(NAMESPACE, seed)`, so a different namespace re-mints
/// every derived entity in the library at once — the failure `AudioId::SCHEME`
/// exists to prevent one layer down.
pub const NAMESPACE: Uuid = Uuid::from_u128(0x9e3f_7a2c_4b81_4d6e_9c05_1f2a_6b8d_3e57);

/// A durable identity. Travels between devices, survives a rebuild.
pub struct Id<T> {
    uuid: Uuid,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Id<T> {
    /// Mint a fresh identity, unique without coordinating with anyone.
    ///
    /// For entities with no external binding to derive from. Where a binding
    /// exists — an audio hash, a MusicBrainz id — [`Id::derived`] is used
    /// instead so that two devices agree without having met (#66).
    pub fn new() -> Self {
        Self::from_uuid(Uuid::new_v4())
    }

    /// Mint the identity that these bytes always mint, on any machine.
    ///
    /// The whole point is that it is *not* random: two devices scanning the
    /// same file arrive at the same id without talking to each other, which is
    /// what `library-sync-design.md` §2 requires of anything that syncs.
    pub fn derived(seed: &[u8]) -> Self {
        Self::from_uuid(Uuid::new_v5(&NAMESPACE, seed))
    }

    /// Wrap a UUID that is already an identity — a MusicBrainz id being read
    /// back, or a value from storage.
    pub const fn from_uuid(uuid: Uuid) -> Self {
        Id {
            uuid,
            _marker: PhantomData,
        }
    }

    pub const fn as_uuid(&self) -> Uuid {
        self.uuid
    }
}

impl<T> Default for Id<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// An index into an arena. **Process-local**, invalidated by a rebuild.
///
/// Deliberately not serialisable. That absence is the enforcement of the
/// portability invariant, not an oversight: if a handle could be persisted,
/// a rebuild that reorders the arena would silently reattach every stored one
/// to a different entity.
pub struct Handle<T> {
    index: u32,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Handle<T> {
    pub const fn new(index: u32) -> Self {
        Handle {
            index,
            _marker: PhantomData,
        }
    }

    pub const fn index(&self) -> u32 {
        self.index
    }
}

/// A reference to an entity, which *may* know where it currently lives.
///
/// **A reference is valid when its target is absent.** A recording with no
/// distribution, or a release not currently loaded, is ordinary — resolution
/// is an optimisation, not a precondition.
pub struct Ref<T> {
    id: Id<T>,
    resolved: Option<Handle<T>>,
}

impl<T> Ref<T> {
    pub const fn to(id: Id<T>) -> Self {
        Ref { id, resolved: None }
    }

    pub const fn id(&self) -> Id<T> {
        self.id
    }

    /// The handle, if this reference has been resolved in this process.
    pub const fn handle(&self) -> Option<Handle<T>> {
        self.resolved
    }

    /// Record where the target currently sits, for the life of this process.
    pub fn resolve(&mut self, handle: Handle<T>) {
        self.resolved = Some(handle);
    }

    /// Forget the resolution. What a rebuild does to every reference.
    pub fn unresolve(&mut self) {
        self.resolved = None;
    }
}

impl<T> From<Id<T>> for Ref<T> {
    fn from(id: Id<T>) -> Self {
        Ref::to(id)
    }
}

// ---------------------------------------------------------------------------
// Hand-written impls. `derive` would add a `T:` bound on a marker that is never
// constructed, so `Id<Recording>` would demand `Recording: Clone` to be cloned.
// ---------------------------------------------------------------------------

macro_rules! phantom_impls {
    ($name:ident, $field:ident) => {
        impl<T> Clone for $name<T> {
            fn clone(&self) -> Self {
                *self
            }
        }
        impl<T> Copy for $name<T> {}
        impl<T> PartialEq for $name<T> {
            fn eq(&self, other: &Self) -> bool {
                self.$field == other.$field
            }
        }
        impl<T> Eq for $name<T> {}
        impl<T> PartialOrd for $name<T> {
            fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
                Some(self.cmp(other))
            }
        }
        impl<T> Ord for $name<T> {
            fn cmp(&self, other: &Self) -> Ordering {
                self.$field.cmp(&other.$field)
            }
        }
        impl<T> Hash for $name<T> {
            fn hash<H: Hasher>(&self, state: &mut H) {
                self.$field.hash(state);
            }
        }
    };
}

phantom_impls!(Id, uuid);
phantom_impls!(Handle, index);

impl<T> Clone for Ref<T> {
    fn clone(&self) -> Self {
        *self
    }
}
impl<T> Copy for Ref<T> {}

/// Two references are equal when they name the same entity. Whether either
/// happens to be resolved is a fact about this process, not about the
/// reference.
impl<T> PartialEq for Ref<T> {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl<T> Eq for Ref<T> {}
impl<T> Hash for Ref<T> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state);
    }
}

impl<T> fmt::Debug for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Id({})", self.uuid)
    }
}
impl<T> fmt::Display for Id<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.uuid, f)
    }
}
impl<T> fmt::Debug for Handle<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Handle({})", self.index)
    }
}
impl<T> fmt::Debug for Ref<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.resolved {
            Some(h) => write!(f, "Ref({} -> {})", self.id.uuid, h.index),
            None => write!(f, "Ref({})", self.id.uuid),
        }
    }
}

/// Why a string is not an id.
#[derive(Debug, thiserror::Error)]
#[error("not an id: {0}")]
pub struct ParseIdError(String);

impl<T> std::str::FromStr for Id<T> {
    type Err = ParseIdError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Uuid::parse_str(s)
            .map(Id::from_uuid)
            .map_err(|_| ParseIdError(s.to_string()))
    }
}

// ---------------------------------------------------------------------------
// serde. `bound = ""` for the same reason the impls above are hand-written.
// `Handle` has none, deliberately — see its doc comment.
// ---------------------------------------------------------------------------

impl<T> serde::Serialize for Id<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.uuid.serialize(s)
    }
}

impl<'de, T> serde::Deserialize<'de> for Id<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Uuid::deserialize(d).map(Id::from_uuid)
    }
}

/// A reference persists as **the id alone**.
///
/// The resolution is process-local, so a round trip through storage
/// deliberately loses it — which is the portability invariant made
/// operational rather than merely documented.
impl<T> serde::Serialize for Ref<T> {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.id.serialize(s)
    }
}

impl<'de, T> serde::Deserialize<'de> for Ref<T> {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Id::deserialize(d).map(Ref::to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Marker types. They are never constructed and implement nothing, which is
    // the point: the impls above must not demand anything of them.
    struct Recording;
    struct Release;

    #[test]
    fn a_minted_id_is_unique() {
        assert_ne!(Id::<Recording>::new(), Id::<Recording>::new());
    }

    /// The whole reason for the namespace: two devices, no coordination, one
    /// id. Convergent minting (#66) is this and nothing else.
    #[test]
    fn a_derived_id_is_the_same_on_every_machine() {
        let a = Id::<Recording>::derived(b"v1:8fb0dcfbf145dfff6649784265da0229");
        let b = Id::<Recording>::derived(b"v1:8fb0dcfbf145dfff6649784265da0229");
        assert_eq!(a, b, "the same seed must mint the same id");
        assert_ne!(a, Id::<Recording>::derived(b"something else"));
    }

    /// **The guard against a silent namespace change.** Every derived id in
    /// every log depends on `NAMESPACE`, so a change re-mints the library.
    #[test]
    fn the_namespace_is_pinned() {
        assert_eq!(
            Id::<Recording>::derived(b"gmuse").to_string(),
            "bbd4433a-ac75-57ce-9db2-26115a92645e",
            "NAMESPACE changed: every derived id ever written is now wrong"
        );
    }

    #[test]
    fn an_id_round_trips_through_its_text() {
        let id = Id::<Recording>::new();
        assert_eq!(id.to_string().parse::<Id<Recording>>().unwrap(), id);
        assert!("not a uuid".parse::<Id<Recording>>().is_err());
    }

    /// A reference is about its target, not about whether this process has
    /// happened to look it up.
    #[test]
    fn resolution_does_not_change_what_a_reference_names() {
        let id = Id::<Recording>::new();
        let bare = Ref::to(id);
        let mut resolved = Ref::to(id);
        resolved.resolve(Handle::new(7));

        assert_eq!(bare, resolved);
        assert_eq!(bare.id(), resolved.id());
        assert_eq!(resolved.handle().map(|h| h.index()), Some(7));
        assert_eq!(bare.handle(), None, "an unresolved reference is ordinary");
    }

    /// **The portability invariant, made operational.** A handle cannot be
    /// serialised, so a stored reference can only be an id — and coming back
    /// from storage it is unresolved, exactly as it would be after a rebuild.
    #[test]
    fn a_reference_persists_as_the_id_alone() {
        let mut r = Ref::<Recording>::to(Id::new());
        r.resolve(Handle::new(3));

        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: Ref<Recording> = postcard::from_bytes(&bytes).unwrap();

        assert_eq!(back, r, "it still names the same entity");
        assert_eq!(
            back.handle(),
            None,
            "an arena index must never survive storage"
        );
        assert_eq!(
            bytes,
            postcard::to_allocvec(&r.id()).unwrap(),
            "a reference costs exactly an id on disk"
        );
    }

    /// A rebuild invalidates every handle, and saying so is one call.
    #[test]
    fn unresolve_is_what_a_rebuild_does() {
        let mut r = Ref::<Recording>::to(Id::new());
        r.resolve(Handle::new(1));
        r.unresolve();
        assert_eq!(r.handle(), None);
    }

    /// The tag is the whole point: these are different types, and the compiler
    /// keeps them apart at no runtime cost.
    #[test]
    fn a_tagged_id_costs_nothing() {
        assert_eq!(
            std::mem::size_of::<Id<Recording>>(),
            std::mem::size_of::<Uuid>()
        );
        assert_eq!(
            std::mem::size_of::<Id<Recording>>(),
            std::mem::size_of::<Id<Release>>()
        );
        assert_eq!(std::mem::size_of::<Handle<Recording>>(), 4);
    }

    /// Markers implement nothing. If any impl above grew a `T:` bound this
    /// would stop compiling, which is the test.
    #[test]
    fn the_marker_type_is_never_required_to_do_anything() {
        use std::collections::HashSet;
        let id = Id::<Recording>::new();
        let copied = id;
        let mut set = HashSet::new();
        set.insert(copied);
        let mut ids = [Id::<Recording>::new(), id];
        ids.sort();
        assert!(set.contains(&id) && ids.len() == 2);
    }
}
