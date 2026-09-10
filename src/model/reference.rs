//! The reference layer — issue #73, `domain-model.md` §2.
//!
//! What the containment spine points at: artists, the roles they played, the
//! names they have been called, and how they relate to each other. Maintained
//! artefacts rather than observations — the things that stay put while
//! recordings come and go.

use std::collections::BTreeMap;

use crate::model::entity::{Artist, Concept, Label, Recording, Release, ReleaseGroup, Role, Work};
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
    Concept(Id<Concept>),
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

impl Id<Label> {
    /// The id an imprint's name always mints.
    ///
    /// Through [`crate::model::ingest::normalise`], like every other concept:
    /// two devices reading the same sleeve land on the same imprint without
    /// meeting. 218 distinct labels here, and `Columbia` on one file must be
    /// `Columbia` on the next.
    pub fn of_name(name: &str) -> Self {
        Id::derived(crate::model::ingest::normalise(name).as_bytes())
    }
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
    /// Why this credit is recorded — #152.
    ///
    /// Carried so a **line can be superseded rather than appended to**. It was
    /// dropped in the projection, and the fold keys credits by it, so a
    /// correction arrived as an extra name: splitting
    /// `Stephan Bodzin & Marc Romboy` printed
    /// `Stephan Bodzin & Marc Romboy, Stephan Bodzin, Marc Romboy`, and since
    /// `file_under` is this line, the tree would have followed it.
    pub basis: Source,
}

/// Why a name is recorded, and how much it should be trusted.
///
/// Load-bearing rather than decorative. An alias minted from a tag records what
/// **one file claimed**, not that the spelling is legitimate — so a typo that
/// got ingested never acquires the authority of a sleeve credit, and a bad one
/// can be pruned by its basis without touching the aliases that were earned.
/// **The order is precedence, and it is load-bearing** (#96).
///
/// Weakest first, so `Ord` says who wins: a person's judgement beats an
/// import, an import beats an inference, an inference beats a tag. That is
/// what makes a correction survive a resync — without it, retracting a wrong
/// Discogs genre lasts exactly until the next Discogs pass.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
// Lowercase on the wire, because a proposal file is something a person types
// and `"basis":"discogs"` is what they will type. Postcard encodes a variant by
// index rather than by name, so this costs the stored form nothing.
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// **Read off the file's name** — #143, and the weakest rung there is.
    ///
    /// Below a tag deliberately. A path claim is evidence and never the
    /// answer: it is recorded, it is queryable, and while any tag says
    /// anything it does not render. That is the whole discipline — a DJ-pool
    /// filename carries a chart position where a track number goes, so a rule
    /// that let the path win would be right about `_pn` and wrong about `37
    /// Long Chicken` on a two-track release.
    ///
    /// Where the path really is the better record — `storage-layout.md` §4's
    /// ID3v1 stumps — that is settled by MusicBrainz or by a person, not by a
    /// rung. A ladder cannot encode "better on these files and worse on
    /// those".
    Path,
    /// Read off a file. The weakest thing a name can be.
    TagVariant,
    Inferred,
    // Snake case makes this `music_brainz`, which nobody has ever typed. The
    // point of the lowercase wire form is that a proposal file is something a
    // person edits by hand, and a form they have to look up defeats it.
    #[serde(rename = "musicbrainz")]
    MusicBrainz,
    Discogs,
    /// Someone typed it on purpose.
    Manual,
}

impl Source {
    /// What a fact written before its rung was recorded must have been.
    ///
    /// Every `EncodingPlaced` in a log older than #170 was written by a scan,
    /// so reading one back as anything stronger would let history outrank a
    /// decision made today.
    pub fn of_a_tag() -> Self {
        Source::TagVariant
    }
}

impl Source {
    /// How it is written in a filter and in a proposal file — one spelling,
    /// so `genre[basis=discogs]` matches what a tool wrote (#93).
    pub fn written(self) -> &'static str {
        match self {
            Source::Path => "path",
            Source::TagVariant => "tag",
            Source::Inferred => "inferred",
            Source::MusicBrainz => "musicbrainz",
            Source::Discogs => "discogs",
            Source::Manual => "manual",
        }
    }
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
    /// Every credit, grouped by what it is on and ordered by position.
    ///
    /// **Built once, because `credits_on` is a linear scan** (#123). It reads
    /// well and costs nothing when something asks about one album; `draw`
    /// asks about every row in the library, and on 6,500 rows against 13,000
    /// credits that was 6,500 full passes — 333 ms of a 397 ms fold, and the
    /// single largest cost in rebuilding the librarian.
    pub fn by_subject(&self) -> std::collections::BTreeMap<CreditOn, Vec<&Credit>> {
        let mut out: std::collections::BTreeMap<CreditOn, Vec<&Credit>> = Default::default();
        for credit in &self.credits {
            out.entry(credit.subject).or_default().push(credit);
        }
        for found in out.values_mut() {
            found.sort_by_key(|c| c.position);
        }
        out
    }

    pub fn credited_as(&self, subject: CreditOn) -> String {
        self.line_of(&self.credits_on(subject))
    }

    /// The printed line, given the credits already found.
    ///
    /// Split from [`Self::credited_as`] so a caller holding an index can skip
    /// the scan — and so there is still exactly one place that decides how a
    /// credit line reads.
    pub fn line_of(&self, credits: &[&Credit]) -> String {
        // **A credit with no name is not a credit.** An artist the catalogue
        // has no name for resolved to an empty string and still took its join
        // phrase with it, so `Emmylou Harris with The Band` came out as
        // `Emmylou Harris, ` — a trailing separator introducing nobody.
        // Dropped here rather than trimmed afterwards, because the separator
        // belongs to the credit that is missing.
        // **One artist appears once.** Two facts crediting the same artist
        // with different `as_credited` spellings both survive the fold — they
        // are different claims — and joining them produced `Orbital, Orbital`
        // and a Trio listed twice under two spellings of its own line. The
        // first by position wins, which is the one the credit is ordered by.
        // **The best-attested credits supersede the rest** — #152.
        //
        // Credits accumulate and there is no retraction, deliberately: the
        // fold keys them by basis because a remixer guessed from a title and
        // MusicBrainz naming the same remixer are two pieces of evidence, and
        // their agreeing is the most useful thing the pair says. That is right
        // for the *log* and wrong for a *line*, which has to print one answer.
        //
        // Unioned, a correction arrived as an extra name: splitting
        // `Stephan Bodzin & Marc Romboy` printed
        // `Stephan Bodzin & Marc Romboy, Stephan Bodzin, Marc Romboy` — and
        // `file_under` is this line, so the tree would have followed it into a
        // folder by that name.
        //
        // `Source` is ordered from `TagVariant`, "the weakest thing a name can
        // be", to `Manual`, "someone typed it on purpose", so the strongest
        // basis present is the answer and the weaker ones stay evidence.
        // **Only who performed it** — a production credit is not authorship.
        //
        // `line_of` answers "who is this by", and `file_under` is this line, so
        // a remixer in it puts the record in the remixer's folder. Not
        // hypothetical: `Slip Away (Olivier Giacomotto Remix)` is credited to
        // `Cari Golden, Bambook, Mennie` and filed under Giacomotto, who
        // arranged it.
        //
        // `featured` is `Performer` and stays, which is the point of the role
        // existing (#89 §5): a guest is part of who a track is by. `remixer`,
        // `producer` and `composer` are not — composer → work → performance is
        // a view, per `storage-layout.md`, and never a folder.
        //
        // Where nothing performed it, the line is empty and `file_under`
        // renders `Unknown Artist`. Borrowing the producer's name would be a
        // worse kind of wrong than admitting the library does not know.
        let performed = |c: &Credit| {
            self.roles
                .get(&c.role.id())
                .is_some_and(|r| r.class == RoleClass::Performer)
        };
        let Some(best) = credits
            .iter()
            .filter(|c| performed(c))
            .map(|c| c.basis)
            .max()
        else {
            return String::new();
        };
        // **Deduplicated by the rendered name, not by the artist id** — #156.
        //
        // The id rule was the same rule one step too early: an album merged out
        // of two entries brings both their credits, and where one was tagged
        // with a MusicBrainz artist id and the other was not, **the same person
        // is two ids**. The line came out `Anderson .Paak, Anderson .Paak` on
        // 298 files, and `file_under` is this line, so `shelf render` wanted to
        // move every one of them into a folder by that name.
        //
        // Two people who really share a name collapse here, and that is the
        // right trade for a *line*: printing one name twice is unreadable
        // whichever way it happened, and the credits keep both.
        //
        // **A linear scan, and no set.** A credit line is one to five names,
        // and this runs per row while the tree draws thousands: hashing or
        // normalising each name cost more than comparing them all. `normalise`
        // here took a narrowed view from under 50ms to 92.
        let mut named: Vec<(String, Option<String>)> = Vec::new();
        for credit in credits.iter().filter(|c| performed(c) && c.basis == best) {
            let name = credit.as_credited.clone().unwrap_or_else(|| {
                self.artists
                    .get(&credit.artist.id())
                    .map(|a| a.name.clone())
                    .unwrap_or_default()
            });
            if name.trim().is_empty() || named.iter().any(|(seen, _)| *seen == name) {
                continue;
            }
            named.push((name, credit.join_phrase.clone()));
        }

        let mut line = String::new();
        for (i, (name, join)) in named.iter().enumerate() {
            line.push_str(name);
            if i + 1 < named.len() {
                line.push_str(join.as_deref().unwrap_or(", "));
            }
        }
        line
    }

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — alias resolution and credit repair, which no command drives"
    )]
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

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — alias resolution and credit repair, which no command drives"
    )]
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

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — alias resolution and credit repair, which no command drives"
    )]
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

    #[allow(
        dead_code,
        reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — alias resolution and credit repair, which no command drives"
    )]
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

    fn credit(r: &mut Reference, on: CreditOn, who: Id<Artist>, at: u8, basis: Source) {
        let performer = role(r, "performer", RoleClass::Performer);
        r.credits.push(Credit {
            subject: on,
            artist: Ref::to(who),
            role: Ref::to(performer),
            position: at,
            as_credited: None,
            join_phrase: None,
            basis,
        });
    }

    /// **A remixer is not who the record is by.**
    ///
    /// `file_under` is this line, so a production credit in it files the
    /// record in the producer's folder. `Slip Away (Olivier Giacomotto Remix)`
    /// is credited to `Cari Golden, Bambook, Mennie` and landed under
    /// Giacomotto, who arranged it.
    #[test]
    fn a_production_credit_is_not_part_of_who_it_is_by() {
        let mut r = Reference::default();
        let cari = artist(&mut r, "Cari Golden");
        let olivier = artist(&mut r, "Olivier Giacomotto");
        let rec = CreditOn::Recording(Id::derived(b"slip away"));

        let performer = role(&mut r, "performer", RoleClass::Performer);
        let remixer = role(&mut r, "remixer", RoleClass::Production);
        for (who, what, at) in [(cari, performer, 0u8), (olivier, remixer, 1)] {
            r.credits.push(Credit {
                subject: rec,
                artist: Ref::to(who),
                role: Ref::to(what),
                position: at,
                as_credited: None,
                join_phrase: None,
                basis: Source::TagVariant,
            });
        }
        assert_eq!(r.credited_as(rec), "Cari Golden");
    }

    /// A guest **is** part of who a track is by, which is why the role exists.
    #[test]
    fn a_featured_guest_stays_in_the_line() {
        let mut r = Reference::default();
        let pac = artist(&mut r, "2Pac");
        let dogg = artist(&mut r, "Nate Dogg");
        let rec = CreditOn::Recording(Id::derived(b"all bout u"));
        let performer = role(&mut r, "performer", RoleClass::Performer);
        let featured = role(&mut r, "featured", RoleClass::Performer);
        for (who, what, at) in [(pac, performer, 0u8), (dogg, featured, 1)] {
            r.credits.push(Credit {
                subject: rec,
                artist: Ref::to(who),
                role: Ref::to(what),
                position: at,
                as_credited: None,
                join_phrase: None,
                basis: Source::TagVariant,
            });
        }
        assert_eq!(r.credited_as(rec), "2Pac, Nate Dogg");
    }

    /// Nothing performed it, so the library says nothing rather than borrowing
    /// the producer's name.
    #[test]
    fn a_subject_with_only_production_credits_has_no_line() {
        let mut r = Reference::default();
        let olivier = artist(&mut r, "Olivier Giacomotto");
        let rec = CreditOn::Recording(Id::derived(b"only a remix"));
        let remixer = role(&mut r, "remixer", RoleClass::Production);
        r.credits.push(Credit {
            subject: rec,
            artist: Ref::to(olivier),
            role: Ref::to(remixer),
            position: 0,
            as_credited: None,
            join_phrase: None,
            basis: Source::TagVariant,
        });
        assert_eq!(r.credited_as(rec), "");
    }

    /// **A correction replaces the line; it does not join it** — #152.
    ///
    /// Credits accumulate and nothing retracts them, so a split arrived beside
    /// the joined credit it fixes and printed all three names. `file_under` is
    /// this line, so a `shelf render` would have moved 285 files into folders
    /// named `Stephan Bodzin & Marc Romboy, Stephan Bodzin, Marc Romboy`.
    #[test]
    fn a_stronger_basis_supersedes_the_line_rather_than_extending_it() {
        let mut r = Reference::default();
        let joined = artist(&mut r, "Stephan Bodzin & Marc Romboy");
        let bodzin = artist(&mut r, "Stephan Bodzin");
        let romboy = artist(&mut r, "Marc Romboy");
        let group = CreditOn::ReleaseGroup(Id::derived(b"luna"));

        credit(&mut r, group, joined, 0, Source::TagVariant);
        assert_eq!(r.credited_as(group), "Stephan Bodzin & Marc Romboy");

        credit(&mut r, group, bodzin, 0, Source::Inferred);
        credit(&mut r, group, romboy, 1, Source::Inferred);
        assert_eq!(
            r.credited_as(group),
            "Stephan Bodzin, Marc Romboy",
            "the tag reading is evidence now, not half of the answer"
        );
    }

    /// **One person, two ids, one name in the line** — #156.
    ///
    /// The line deduplicated by artist *id*, which is the same rule one step
    /// too early. Merging two album entries into one brings both their
    /// credits, and where one was tagged with a MusicBrainz artist id and the
    /// other was not, the same person is two ids: the line came out
    /// `Anderson .Paak, Anderson .Paak` on 298 files. `file_under` is this
    /// line, so `shelf render` wanted to move every one of them into a folder
    /// by that name.
    #[test]
    fn one_person_credited_under_two_ids_is_named_once() {
        let mut r = Reference::default();
        let group = CreditOn::ReleaseGroup(Id::derived(b"malibu"));
        // The two ids `of_artist` mints for one person, depending on whether
        // the file that made them carried a MusicBrainz id.
        let named = Id::<Artist>::derived(b"anderson-paak-mbid");
        let minted = Id::<Artist>::derived(b"anderson-paak-from-the-string");
        for id in [named, minted] {
            r.artists.insert(
                id,
                ArtistRecord {
                    id,
                    name: "Anderson .Paak".into(),
                    sort_name: "Paak, Anderson .".into(),
                },
            );
        }
        credit(&mut r, group, named, 0, Source::TagVariant);
        credit(&mut r, group, minted, 1, Source::TagVariant);

        assert_eq!(r.credited_as(group), "Anderson .Paak");

        // And two people who really are two are both still named.
        let other = artist(&mut r, "Knxwledge");
        credit(&mut r, group, other, 2, Source::TagVariant);
        assert_eq!(r.credited_as(group), "Anderson .Paak, Knxwledge");
    }

    /// The weaker credits are still *there* — superseded is not deleted.
    #[test]
    fn the_superseded_credits_remain_in_the_reference() {
        let mut r = Reference::default();
        let joined = artist(&mut r, "A & B");
        let a = artist(&mut r, "A");
        let group = CreditOn::ReleaseGroup(Id::derived(b"x"));
        credit(&mut r, group, joined, 0, Source::TagVariant);
        credit(&mut r, group, a, 0, Source::Manual);
        assert_eq!(r.credits_on(group).len(), 2, "both are held");
        assert_eq!(r.credited_as(group), "A", "one is printed");
    }

    /// One basis, several artists: the ordinary case must be untouched.
    #[test]
    fn credits_at_one_basis_all_print() {
        let mut r = Reference::default();
        let (a, b) = (artist(&mut r, "A"), artist(&mut r, "B"));
        let rec = CreditOn::Recording(Id::derived(b"r"));
        credit(&mut r, rec, a, 0, Source::TagVariant);
        credit(&mut r, rec, b, 1, Source::TagVariant);
        assert_eq!(r.credited_as(rec), "A, B");
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
            basis: Source::TagVariant,
        });
        r.credits.push(Credit {
            subject: rec,
            artist: Ref::to(romboy),
            role: Ref::to(performer),
            position: 1,
            as_credited: None,
            join_phrase: None,
            basis: Source::TagVariant,
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
            basis: Source::TagVariant,
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
            basis: Source::TagVariant,
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
            basis: Source::TagVariant,
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
            basis: Source::TagVariant,
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
            basis: Source::TagVariant,
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
