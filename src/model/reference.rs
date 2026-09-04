//! The reference layer — issue #73, `domain-model.md` §2.
//!
//! What the containment spine points at: artists, the roles they played, the
//! names they have been called, and how they relate to each other. Maintained
//! artefacts rather than observations — the things that stay put while
//! recordings come and go.

use std::collections::BTreeMap;

use crate::model::entity::{Artist, Genre, Label, Recording, Release, ReleaseGroup, Role, Work};
use crate::model::id::{Id, Ref};
use crate::model::spine::PartialDate;

/// Anything the catalogue holds an opinion about.
///
/// One sum type, so provenance, aliases and external identifiers each need one
/// mechanism rather than one per kind. `Role` and `Label` are in it for
/// concrete reasons: a provisional role minted at ingest needs somewhere to
/// record that it was a guess, and `Warp` against `Warp Records` is an alias
/// problem exactly as `Emmy Lou Harris` is.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum Cataloguable {
    Work(Id<Work>),
    Recording(Id<Recording>),
    Release(Id<Release>),
    ReleaseGroup(Id<ReleaseGroup>),
    Artist(Id<Artist>),
    Genre(Id<Genre>),
    Role(Id<Role>),
    Label(Id<Label>),
}

/// A person or an ensemble.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtistRecord {
    pub id: Id<Artist>,
    /// The label. **Ours** — seeded from a binding, never owned by one, so an
    /// upstream rename is a fact to adopt rather than one that happens to us.
    pub name: String,
    pub sort_name: String,
}

/// An imprint.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct LabelRecord {
    pub id: Id<Label>,
    pub name: String,
}

/// What behaviour branches on, where a role's *name* never should.
///
/// Roles proliferate — remixer, conductor, soloist, arranger, "additional
/// production" — and the player almost never cares which. Where it must care,
/// it cares about this, which is closed and compiled in. That is what keeps
/// "composer credits belong on a work" enforceable while the list stays open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum RoleClass {
    Composer,
    Performer,
    Production,
}

/// The roles this codelist seeds with.
///
/// Small on purpose. A codelist grows by fact — `RoleDefined` will take any
/// name — so what belongs here is the set an *ingest* can produce without
/// guessing, plus the two a title parse already recognises.
///
/// `featured` is separate from `performer` because a guest credit is the
/// thing that used to split an artist: `2Pac feat. Nate Dogg` minted a
/// fifteenth 2Pac, and the fix was to take the primary id — which leaves the
/// guest with nowhere to be recorded unless the role exists.
pub const ROLES: &[(&str, RoleClass)] = &[
    ("performer", RoleClass::Performer),
    ("featured", RoleClass::Performer),
    ("remixer", RoleClass::Production),
    ("producer", RoleClass::Production),
    ("composer", RoleClass::Composer),
];

impl Id<Role> {
    /// The id a role name always mints.
    ///
    /// Through [`crate::model::ingest::normalise`], for the reason concept ids
    /// are: the codelist has to be definable on two machines that never meet.
    /// It replaces `Id::derived(b"performer")` written inline in the
    /// projection, which was one role, minted once, handed to everybody.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::model::ingest::normalise(name).as_bytes())
    }
}

/// What an artist did, as a codelist entry.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RoleRecord {
    pub id: Id<Role>,
    pub name: String,
    pub class: RoleClass,
}

/// What a credit is *on*.
///
/// A sum type rather than four optional references, so "credited to nothing"
/// and "credited to both a work and a release" cannot be written down.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub enum CreditOn {
    Work(Id<Work>),
    Recording(Id<Recording>),
    ReleaseGroup(Id<ReleaseGroup>),
    Release(Id<Release>),
}

/// An artist on something, in a role, at a position.
///
/// It reaches up to the album levels because *browse by album artist* is the
/// most-used level in any player: aggregating a release's recording credits is
/// a guess, and it leaves various-artists with no authoritative marker.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Credit {
    pub subject: CreditOn,
    pub artist: Ref<Artist>,
    pub role: Ref<Role>,
    pub position: u8,
    /// What the sleeve printed for **this artist**, when it differs from the
    /// label.
    pub as_credited: Option<String>,
    /// What joins this credit to the next — `" & "`, `" vs. "`, `" feat. "`.
    pub join_phrase: Option<String>,
}

/// Why a name is recorded, and how much it should be trusted.
///
/// Load-bearing rather than decorative. An alias minted from a tag records what
/// **one file claimed**, not that the spelling is legitimate — so a typo that
/// got ingested never acquires the authority of a sleeve credit, and a bad one
/// can be pruned by its basis without touching the aliases that were earned.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
// Lowercase on the wire, because a proposal file is something a person types
// and `"basis":"discogs"` is what they will type. Postcard encodes a variant by
// index rather than by name, so this costs the stored form nothing.
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Read off a file. The weakest thing a name can be.
    TagVariant,
    Inferred,
    MusicBrainz,
    Discogs,
    /// Someone typed it on purpose.
    Manual,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub enum AliasKind {
    /// Another way the world spells it.
    SearchHint,
    LegalName,
    /// The name a merge retired.
    Retired,
}

/// Another name for something.
///
/// Authority control as libraries have always done it — an authorised heading
/// with see-references — and `skos:altLabel` without the RDF.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Alias {
    pub subject: Cataloguable,
    pub value: String,
    pub kind: AliasKind,
    pub basis: Source,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ArtistRelationKind {
    MemberOf,
    AliasOf,
}

/// An artist to an artist.
///
/// The composer tag in this library is mostly naming band members and
/// pseudonyms rather than cover originals — `Black Francis` on 40 Pixies files,
/// `Paul Hartnoll/Phil Hartnoll` on 34 Orbital files, `Marc Leclair` on Akufen.
/// "Everything by this artist under any name" is a first-order question here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtistRelation {
    pub from: Ref<Artist>,
    pub to: Ref<Artist>,
    pub kind: ArtistRelationKind,
    pub period: Option<(Option<PartialDate>, Option<PartialDate>)>,
}

/// The maintained artefacts, held flat.
#[derive(Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(bound = "")]
pub struct Reference {
    pub artists: BTreeMap<Id<Artist>, ArtistRecord>,
    pub labels: BTreeMap<Id<Label>, LabelRecord>,
    pub roles: BTreeMap<Id<Role>, RoleRecord>,
    pub credits: Vec<Credit>,
    pub aliases: Vec<Alias>,
    pub artist_relations: Vec<ArtistRelation>,
}

impl Reference {
    /// The credits on something, in the order the sleeve printed them.
    pub fn credits_on(&self, subject: CreditOn) -> Vec<&Credit> {
        let mut found: Vec<_> = self
            .credits
            .iter()
            .filter(|c| c.subject == subject)
            .collect();
        found.sort_by_key(|c| c.position);
        found
    }

    /// Rebuild the line as printed.
    ///
    /// Display reads `as_credited ?? name`; grouping and search always resolve
    /// through the reference, so a printed form never fragments the artist
    /// dimension.
    pub fn credited_as(&self, subject: CreditOn) -> String {
        let credits = self.credits_on(subject);
        let mut line = String::new();
        for (i, c) in credits.iter().enumerate() {
            let name = c.as_credited.clone().unwrap_or_else(|| {
                self.artists
                    .get(&c.artist.id())
                    .map(|a| a.name.clone())
                    .unwrap_or_default()
            });
            line.push_str(&name);
            if i + 1 < credits.len() {
                line.push_str(c.join_phrase.as_deref().unwrap_or(", "));
            }
        }
        line
    }

    /// Find an artist by any name it is known under, preferring the label and
    /// then the most trustworthy alias.
    ///
    /// **This is how the matcher learns.** A file tagged `Emmy Lou Harris` with
    /// no MusicBrainz id resolves here rather than falling to a provisional,
    /// because a previous resolution left the variant behind. Every merge and
    /// every rename makes the next resolution stronger.
    pub fn artist_named(&self, name: &str) -> Option<Id<Artist>> {
        let wanted = crate::model::ingest::normalise(name);
        if let Some(a) = self
            .artists
            .values()
            .find(|a| crate::model::ingest::normalise(&a.name) == wanted)
        {
            return Some(a.id);
        }
        self.aliases
            .iter()
            .filter(|al| crate::model::ingest::normalise(&al.value) == wanted)
            .filter_map(|al| match al.subject {
                Cataloguable::Artist(id) => Some((al.basis, id)),
                _ => None,
            })
            // A sleeve credit outranks something read off one file's tags.
            .max_by_key(|(basis, _)| *basis)
            .map(|(_, id)| id)
    }

    /// Record that this name also means this artist.
    pub fn add_alias(&mut self, artist: Id<Artist>, value: &str, kind: AliasKind, basis: Source) {
        let alias = Alias {
            subject: Cataloguable::Artist(artist),
            value: value.to_string(),
            kind,
            basis,
        };
        if !self.aliases.contains(&alias) {
            self.aliases.push(alias);
        }
    }

    /// Merge one artist into another, keeping the loser's name findable.
    ///
    /// The other half of #68: a redirect keeps every old *reference* resolving,
    /// and this keeps the old *name* resolving. Without it, merging
    /// `Emmy Lou Harris` away means the next file tagged that way cannot be
    /// matched and mints a provisional — the merge would make the matcher
    /// worse.
    pub fn absorb(&mut self, loser: Id<Artist>, winner: Id<Artist>) {
        if let Some(gone) = self.artists.remove(&loser) {
            self.add_alias(winner, &gone.name, AliasKind::Retired, Source::Manual);
        }
        for alias in &mut self.aliases {
            if alias.subject == Cataloguable::Artist(loser) {
                alias.subject = Cataloguable::Artist(winner);
            }
        }
    }

    /// Credits placed where their role class says they cannot go.
    ///
    /// A composition is composed; a performance is performed. Behaviour
    /// branches on the class rather than on the role name, which is what lets
    /// this be checked while the role list stays open.
    pub fn misplaced_credits(&self) -> Vec<&Credit> {
        self.credits
            .iter()
            .filter(|c| {
                let Some(role) = self.roles.get(&c.role.id()) else {
                    return false;
                };
                match (role.class, c.subject) {
                    (RoleClass::Composer, CreditOn::Work(_)) => false,
                    (RoleClass::Composer, _) => true,
                    (RoleClass::Performer, CreditOn::Work(_)) => true,
                    _ => false,
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(r: &mut Reference, name: &str) -> Id<Artist> {
        let id = Id::<Artist>::derived(name.as_bytes());
        r.artists.insert(
            id,
            ArtistRecord {
                id,
                name: name.into(),
                sort_name: name.into(),
            },
        );
        id
    }

    fn role(r: &mut Reference, name: &str, class: RoleClass) -> Id<Role> {
        let id = Id::<Role>::derived(name.as_bytes());
        r.roles.insert(
            id,
            RoleRecord {
                id,
                name: name.into(),
                class,
            },
        );
        id
    }

    /// **The line on the sleeve, rebuilt.** `Stephan Bodzin vs. Marc Romboy` is
    /// two credits joined by `" vs. "`, not one artist with a strange name —
    /// which is why the library holds that act under two spellings today and
    /// splits its play count between them.
    #[test]
    fn a_printed_credit_is_rebuilt_from_its_parts() {
        let mut r = Reference::default();
        let (bodzin, romboy) = (
            artist(&mut r, "Stephan Bodzin"),
            artist(&mut r, "Marc Romboy"),
        );
        let performer = role(&mut r, "performer", RoleClass::Performer);
        let rec = CreditOn::Recording(Id::derived(b"callisto"));

        r.credits.push(Credit {
            subject: rec,
            artist: Ref::to(bodzin),
            role: Ref::to(performer),
            position: 0,
            as_credited: None,
            join_phrase: Some(" vs. ".into()),
        });
        r.credits.push(Credit {
            subject: rec,
            artist: Ref::to(romboy),
            role: Ref::to(performer),
            position: 1,
            as_credited: None,
            join_phrase: None,
        });

        assert_eq!(r.credited_as(rec), "Stephan Bodzin vs. Marc Romboy");
        assert_eq!(
            r.credits_on(rec).len(),
            2,
            "and it is two artists, so both browse under their own name"
        );
    }

    /// A release printing something unusual is a fact about that release, not
    /// about the artist.
    #[test]
    fn a_printed_form_does_not_rename_the_artist() {
        let mut r = Reference::default();
        let dylan = artist(&mut r, "Bob Dylan");
        let performer = role(&mut r, "performer", RoleClass::Performer);
        let rec = CreditOn::Recording(Id::derived(b"one"));
        r.credits.push(Credit {
            subject: rec,
            artist: Ref::to(dylan),
            role: Ref::to(performer),
            position: 0,
            as_credited: Some("Blind Boy Grunt".into()),
            join_phrase: None,
        });

        assert_eq!(r.credited_as(rec), "Blind Boy Grunt", "as printed");
        assert_eq!(
            r.artists[&dylan].name, "Bob Dylan",
            "the artist is untouched, so grouping does not fragment"
        );
    }

    /// **The matcher learns.** A variant that once resolved makes the next
    /// unbound file carrying it resolve too, instead of minting a provisional.
    #[test]
    fn a_resolved_variant_makes_the_next_match_stronger() {
        let mut r = Reference::default();
        let emmylou = artist(&mut r, "Emmylou Harris");
        assert_eq!(r.artist_named("Emmy Lou Harris"), None, "not yet known");

        r.add_alias(
            emmylou,
            "Emmy Lou Harris",
            AliasKind::SearchHint,
            Source::TagVariant,
        );

        assert_eq!(r.artist_named("Emmy Lou Harris"), Some(emmylou));
        assert_eq!(r.artist_named("  emmy lou harris "), Some(emmylou));
    }

    /// A sleeve credit outranks something read off one file's tags, so an
    /// ingested typo cannot outvote a real name.
    #[test]
    fn where_two_aliases_disagree_the_better_source_wins() {
        let mut r = Reference::default();
        let real = artist(&mut r, "Antigone");
        let other = artist(&mut r, "ANTIGONE");
        r.add_alias(
            other,
            "antigone (fr)",
            AliasKind::SearchHint,
            Source::TagVariant,
        );
        r.add_alias(real, "antigone (fr)", AliasKind::SearchHint, Source::Manual);

        assert_eq!(r.artist_named("Antigone (FR)"), Some(real));
    }

    /// **The debt from #68.** A merge keeps the loser's *name* findable, or the
    /// merge makes the matcher worse than it was.
    #[test]
    fn a_merge_keeps_the_retired_name_findable() {
        let mut r = Reference::default();
        let winner = artist(&mut r, "Emmylou Harris");
        let loser = artist(&mut r, "Emmy Lou Harris");
        r.add_alias(
            loser,
            "E. Harris",
            AliasKind::SearchHint,
            Source::TagVariant,
        );

        r.absorb(loser, winner);

        assert!(!r.artists.contains_key(&loser), "one artist now");
        assert_eq!(
            r.artist_named("Emmy Lou Harris"),
            Some(winner),
            "the retired name still finds the survivor"
        );
        assert_eq!(
            r.artist_named("E. Harris"),
            Some(winner),
            "and so do the loser's own aliases"
        );
    }

    /// Behaviour branches on the class, not the name — which is what lets the
    /// rule be enforced while the role list stays open.
    #[test]
    fn a_composer_credited_on_a_performance_is_misplaced() {
        let mut r = Reference::default();
        let bach = artist(&mut r, "J.S. Bach");
        let composer = role(&mut r, "composer", RoleClass::Composer);
        let performer = role(&mut r, "cello", RoleClass::Performer);

        r.credits.push(Credit {
            subject: CreditOn::Work(Id::derived(b"suite")),
            artist: Ref::to(bach),
            role: Ref::to(composer),
            position: 0,
            as_credited: None,
            join_phrase: None,
        });
        assert!(
            r.misplaced_credits().is_empty(),
            "composed on a work is right"
        );

        r.credits.push(Credit {
            subject: CreditOn::Recording(Id::derived(b"rostropovich")),
            artist: Ref::to(bach),
            role: Ref::to(composer),
            position: 0,
            as_credited: None,
            join_phrase: None,
        });
        assert_eq!(
            r.misplaced_credits().len(),
            1,
            "composed on a performance is not"
        );

        r.credits.push(Credit {
            subject: CreditOn::Work(Id::derived(b"suite")),
            artist: Ref::to(bach),
            role: Ref::to(performer),
            position: 1,
            as_credited: None,
            join_phrase: None,
        });
        assert_eq!(
            r.misplaced_credits().len(),
            2,
            "and nobody performs a composition — they perform a recording of it"
        );
    }

    /// Album-level credit is asserted, not aggregated. Without it,
    /// various-artists has no authoritative marker.
    #[test]
    fn a_release_can_be_credited_in_its_own_right() {
        let mut r = Reference::default();
        let va = artist(&mut r, "Various Artists");
        let performer = role(&mut r, "performer", RoleClass::Performer);
        let group = CreditOn::ReleaseGroup(Id::derived(b"80s-hits"));
        r.credits.push(Credit {
            subject: group,
            artist: Ref::to(va),
            role: Ref::to(performer),
            position: 0,
            as_credited: None,
            join_phrase: None,
        });
        assert_eq!(r.credited_as(group), "Various Artists");
    }

    #[test]
    fn the_reference_layer_round_trips_through_storage() {
        let mut r = Reference::default();
        let a = artist(&mut r, "Akufen");
        r.add_alias(a, "Marc Leclair", AliasKind::LegalName, Source::MusicBrainz);
        let horror = artist(&mut r, "Horror Inc.");
        r.artist_relations.push(ArtistRelation {
            from: Ref::to(a),
            to: Ref::to(horror),
            kind: ArtistRelationKind::AliasOf,
            period: None,
        });

        let bytes = postcard::to_allocvec(&r).unwrap();
        let back: Reference = postcard::from_bytes(&bytes).unwrap();
        assert_eq!(back.artist_named("Marc Leclair"), Some(a));
        assert_eq!(back.artist_relations.len(), 1);
    }
}
