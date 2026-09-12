//! **What the catalogue's things are called** — #213, §4.3.
//!
//! `Id<T>`, the UUIDv5 derivation behind it, and the entity kinds it tags.
//!
//! ## Why this is its own crate
//!
//! §4.3 says `gmuse-proto` MUST NOT depend on engine internals; §7.6 wants the
//! ids on the wire to be *real domain identities* rather than strings that
//! happen to look like them. Those two conflict unless the identities live
//! somewhere both the protocol and the library can reach. This is that place,
//! and the alternative — proto redeclaring them — is two declarations of one
//! identity, which drift.
//!
//! ## Why it is not tiny
//!
//! The issue that asked for it expected `Id<T>`, a derivation and a list of
//! kinds. Eight of those kinds are not tags: `Recording` carries a title, a
//! work, a tempo and a length, and `Id<Recording>` names *that* type. Giving
//! this crate marker types of its own would mean two `Recording`s, which is
//! the drift the crate exists to prevent — so the spine and the encoding come
//! too, and the crate is the identity layer rather than an id newtype.
//!
//! What stayed behind is the part that reads a file: `AudioId` is here because
//! it names a row and crosses a wire, while computing one needs a decoder and
//! lives in `gmuse::audio_id`.

pub mod audio_id;
pub mod basis;
pub mod encoding;
pub mod entity;
pub mod id;
pub mod mint;
pub mod minted;
pub mod spine;

/// Fold a name the way every derived id folds it.
///
/// **Part of the identity contract, not a tidying helper.** Two devices that
/// have never met agree on an imprint or a role because they fold its name
/// identically before seeding; change this and every concept ever minted is
/// renamed, everywhere and silently. It sits beside [`id::Id::derived`] for
/// the same reason [`entity::Entity::KIND`] does.
///
/// Named `gmuse::model::ingest::normalise` by every caller, which is where it
/// was written and where it still reads correctly.
pub fn normalise(name: &str) -> String {
    let tidy = |part: &str| part.split_whitespace().collect::<Vec<_>>().join(" ");
    // Empty parts dropped, so a trailing comma does not leave a dangling
    // separator that is itself a distinction.
    name.split(',')
        .map(tidy)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
        .to_lowercase()
}
