//! What a recording's identity was minted from — issue #89.
//!
//! A recording's id is derived, never drawn at random, so two devices that
//! have never met agree about a song without coordinating. **What they derive
//! it from is itself a fact**, and until now it was not recorded anywhere.
//!
//! That absence was the problem. A library where 7.3% of recordings are held
//! together by nothing but their bytes cannot say *which* 7.3%, so the files
//! that a re-encode would strand are invisible, and the list to run a
//! fingerprint over cannot be produced.
//!
//! ## The ladder
//!
//! ```text
//! MusicBrainz   the recording's MBID                convergent, and 92.7% of this library
//! External      an authority and its key            Beatport track ids, a video id
//! Audio         the bytes, and only the bytes       convergent only while nothing is re-encoded
//! ```
//!
//! ## Why the bottom rung is the bytes and not the tags
//!
//! **Prefer the error you can repair.** A false *split* is one merge away from
//! fixed and both histories survive; a false *merge* is permanent, because once
//! two recordings' facts have unioned nothing says which rating belonged to
//! which. There is no split operation and there cannot be a good one.
//!
//! Minting from `artist + title` would converge beautifully and collapse every
//! "Intro", every "Untitled", and every live take beside its studio version —
//! all merges, all unrepairable. Minting from the bytes only ever splits, and a
//! split has a named repair (#88). That asymmetry is the whole argument.
//!
//! ## What this is not
//!
//! Not a confidence, and not a provenance. `Confidence` says how much to
//! believe a *claim*; this says what an *identity* was computed from, which is
//! either true or the id would be a different id.

use uuid::Uuid;

use crate::audio_id::AudioId;
use crate::model::entity::Recording;
use crate::model::id::Id;

/// What a recording's id was derived from.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize)]
pub enum Basis {
    /// A MusicBrainz recording id. The top rung, and most of this library.
    MusicBrainz(Uuid),
    /// Some other authority that names this performance — a Beatport track id,
    /// a video id. **The key is opaque here**: what makes it useful is that two
    /// devices reading the same tag derive the same recording, not that gmuse
    /// understands the authority.
    External { authority: String, key: String },
    /// The bytes. Convergent for identical bytes and nothing more, which is
    /// why a recording resting on this is the one a re-encode strands.
    Audio(AudioId),
}

impl Basis {
    /// Which rung this is, for reporting and for sorting a work list.
    pub const fn rung(&self) -> u8 {
        match self {
            Basis::MusicBrainz(_) => 1,
            Basis::External { .. } => 2,
            Basis::Audio(_) => 3,
        }
    }

    /// Whether nothing but the bytes holds this together — the list a
    /// fingerprint pass exists to shorten.
    pub const fn is_only_bytes(&self) -> bool {
        matches!(self, Basis::Audio(_))
    }

    /// The seed this basis mints from.
    ///
    /// **Domain-separated by the authority's name**, so a Beatport id and a
    /// video id that happen to be the same digits are different recordings.
    /// `Id::derived` already separates by entity kind; this separates within
    /// the kind, which that cannot do.
    fn seed(&self) -> Vec<u8> {
        match self {
            Basis::MusicBrainz(mbid) => {
                let mut s = b"musicbrainz\x1f".to_vec();
                s.extend_from_slice(mbid.as_bytes());
                s
            }
            Basis::External { authority, key } => format!("{authority}\u{1f}{key}").into_bytes(),
            Basis::Audio(audio) => {
                let mut s = b"audio\x1f".to_vec();
                s.extend_from_slice(audio.to_string().as_bytes());
                s
            }
        }
    }
}

impl Id<Recording> {
    /// The identity this basis always mints, on any machine.
    ///
    /// **The one place a recording id is made.** Scattering the derivation was
    /// how the ladder came to live half in `ingest` and half in `history`, with
    /// no way to ask what a given id had been derived from.
    pub fn of_basis(basis: &Basis) -> Self {
        Id::derived(&basis.seed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn audio(hash: u128) -> AudioId {
        AudioId { scheme: 1, hash }
    }

    fn mb(n: u128) -> Basis {
        Basis::MusicBrainz(Uuid::from_u128(n))
    }

    /// The no-coordinator bet: the same basis mints the same id anywhere.
    #[test]
    fn one_basis_one_identity() {
        assert_eq!(Id::of_basis(&mb(1)), Id::of_basis(&mb(1)));
        assert_ne!(Id::of_basis(&mb(1)), Id::of_basis(&mb(2)));
    }

    /// **Different rungs never collide.** A recording minted from an MBID and
    /// one minted from bytes are different entities even in the freak case
    /// where the seeds share bytes, because the rung is part of the seed.
    #[test]
    fn the_rungs_are_separated() {
        let same = "1f0e3dad99908345f7439f8ffabdffc4";
        let a = Basis::External {
            authority: "beatport".into(),
            key: same.into(),
        };
        let b = Basis::External {
            authority: "youtube".into(),
            key: same.into(),
        };
        assert_ne!(
            Id::of_basis(&a),
            Id::of_basis(&b),
            "two authorities sharing a key became one recording"
        );
    }

    /// A separator that can be walked around is not a separator: without one,
    /// `("beat", "port1")` and `("beatport", "1")` are the same seed.
    #[test]
    fn an_authority_cannot_run_into_its_key() {
        let a = Basis::External {
            authority: "beat".into(),
            key: "port1".into(),
        };
        let b = Basis::External {
            authority: "beatport".into(),
            key: "1".into(),
        };
        assert_ne!(Id::of_basis(&a), Id::of_basis(&b));
    }

    /// The residue is the thing that has to be findable, since the whole point
    /// of recording a basis is being able to ask for this list.
    #[test]
    fn only_the_bottom_rung_is_bytes_alone() {
        assert!(Basis::Audio(audio(1)).is_only_bytes());
        assert!(!mb(1).is_only_bytes());
        assert!(
            !Basis::External {
                authority: "beatport".into(),
                key: "9728882".into()
            }
            .is_only_bytes()
        );
    }

    /// Rungs sort so a work list can be ordered worst-first.
    #[test]
    fn the_ladder_is_ordered() {
        assert!(mb(1).rung() < Basis::Audio(audio(1)).rung());
        assert!(
            Basis::External {
                authority: "beatport".into(),
                key: "1".into()
            }
            .rung()
                < Basis::Audio(audio(1)).rung()
        );
    }

    /// The literals every recording id in every log depends on.
    #[test]
    fn the_minting_is_pinned() {
        assert_eq!(
            Id::of_basis(&mb(0x1831_05b5)).to_string(),
            "3ef7559b-67d5-5527-b597-f78bfec60a21",
            "recording minting changed: every stored id is now wrong"
        );
        assert_eq!(
            Id::of_basis(&Basis::Audio(audio(0x8fb0_dcfb))).to_string(),
            "b270ea71-73ba-515f-bd01-8bcac579ecd1",
            "byte-derived minting changed"
        );
    }
}

/// What a library rests on, and where it is thin.
///
/// The report `--migrate-log` and a scan can both print, and the thing that
/// was impossible before a basis was recorded: **which** recordings are held
/// together by nothing but their bytes.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Census {
    pub musicbrainz: usize,
    pub external: usize,
    /// Recordings a re-encode would strand. The work list for #57.
    pub bytes_only: Vec<Id<Recording>>,
}

impl Census {
    pub fn of<'a>(identified: impl IntoIterator<Item = (&'a Id<Recording>, &'a Basis)>) -> Self {
        let mut c = Census::default();
        for (id, basis) in identified {
            match basis {
                Basis::MusicBrainz(_) => c.musicbrainz += 1,
                Basis::External { .. } => c.external += 1,
                Basis::Audio(_) => c.bytes_only.push(*id),
            }
        }
        c
    }

    pub fn total(&self) -> usize {
        self.musicbrainz + self.external + self.bytes_only.len()
    }
}

impl std::fmt::Display for Census {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let n = self.total();
        let share = |k: usize| {
            if n == 0 {
                0.0
            } else {
                100.0 * k as f64 / n as f64
            }
        };
        write!(
            f,
            "{n} recordings — {} by MusicBrainz ({:.1}%), {} by another authority, \
             {} by their bytes alone ({:.1}%)",
            self.musicbrainz,
            share(self.musicbrainz),
            self.external,
            self.bytes_only.len(),
            share(self.bytes_only.len()),
        )
    }
}

#[cfg(test)]
mod census_tests {
    use super::*;
    use std::collections::BTreeMap;

    fn rec(n: u128) -> Id<Recording> {
        Id::from_uuid(Uuid::from_u128(n))
    }

    /// **The question that could not be asked before.** Not "how many rest on
    /// their bytes" — a count was always derivable by subtraction — but *which
    /// ones*, because that is the list a fingerprint pass runs over.
    #[test]
    fn the_residue_is_named_and_not_merely_counted() {
        let held: BTreeMap<Id<Recording>, Basis> = [
            (rec(1), Basis::MusicBrainz(Uuid::from_u128(10))),
            (rec(2), Basis::Audio(AudioId { scheme: 1, hash: 7 })),
            (
                rec(3),
                Basis::External {
                    authority: "beatport".into(),
                    key: "9728882".into(),
                },
            ),
            (rec(4), Basis::Audio(AudioId { scheme: 1, hash: 9 })),
        ]
        .into();

        let c = Census::of(held.iter());
        assert_eq!((c.musicbrainz, c.external), (1, 2 - 1));
        assert_eq!(
            c.bytes_only,
            vec![rec(2), rec(4)],
            "the residue has to come back as identities, not a number"
        );
        assert_eq!(c.total(), 4);
    }

    /// An empty library reports nothing rather than dividing by zero.
    #[test]
    fn an_empty_library_has_no_shares() {
        let c = Census::of(std::iter::empty());
        assert_eq!(c.total(), 0);
        assert!(c.to_string().contains("0 recordings"));
    }
}
