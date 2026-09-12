//! Which version of a song this is — issue #101.
//!
//! A qualifier is **an assertion about a recording, drawn from a small concept
//! scheme, carrying a basis and a confidence**. Not a field, not an enum, not
//! an attribute on the work link.
//!
//! ## Why the recording, and not the work link
//!
//! MusicBrainz hangs `instrumental` on the recording→work relationship, and
//! that placement cannot be copied here because it costs something MusicBrainz
//! does not pay: **7.3% of this library has no work**, and some never will. A
//! fact on the work link cannot mark an instrumental white label at all. A
//! fact on the recording marks it like anything else.
//!
//! The objection — that "instrumental" is meaningless without a work —
//! overstates. It is meaningful relative to the work *as understood*: `212` is
//! known to have lyrics without a `Work` row existing anywhere. What the
//! missing link costs is only the *inference* that a vocal sibling exists.
//!
//! ## A separate scheme from genre
//!
//! Sharing a list would make `genre = instrumental` expressible, and it is not
//! a genre. The two answer different questions and are curated from different
//! evidence: a genre says what kind of music this is, a qualifier says which
//! version of it you are holding.
//!
//! ## Set-valued by construction
//!
//! A recording can carry `{live, instrumental}`. That falls out of assignments
//! being rows rather than a field, and it is why the query language's default
//! quantification has to be stated rather than discovered.

/// The concepts this scheme seeds with.
///
/// **Grouping concepts are marked, and are not assignable.** `mix-variant` and
/// `performance-variant` exist to be closed over — `qualifier+ = mix-variant`
/// — and asserting one directly says nothing about a recording. That is SKOS's
/// Concept-versus-Collection distinction, and the seed marks it rather than
/// leaving `qualifier = mix-variant` to return an empty set and look like an
/// answer.
pub const SEED: &[(&str, Option<&str>, bool)] = &[
    // (label, parent, assignable)
    ("mix-variant", None, false),
    ("instrumental", Some("mix-variant"), true),
    ("acappella", Some("mix-variant"), true),
    ("clean", Some("mix-variant"), true),
    ("dub-plate", Some("mix-variant"), true),
    ("performance-variant", None, false),
    ("live", Some("performance-variant"), true),
    ("demo", Some("performance-variant"), true),
    // **A length variant is a version too**, and leaving it out made "no radio
    // edits in this playlist" unaskable.
    //
    // It was left out on the grounds that a radio edit is a *derivation* — a
    // different length — rather than a version of this recording. That
    // conflated two jobs. The duration tolerances decide whether two things
    // are the same recording; this vocabulary describes what you are holding,
    // and a radio edit is a perfectly good description of it.
    //
    // The real objection is `RecordingDerivation { kind: Edit }` saying the
    // same thing, and it resolves the way `instrumental` already did: a
    // derivation names **both** ends, a qualifier names one. "This is a
    // shortened version of something" is always available; "of *that*" only
    // when you hold both.
    ("length-variant", None, false),
    ("radio-edit", Some("length-variant"), true),
    ("extended-mix", Some("length-variant"), true),
    ("partial", None, true),
    // **"This is the original", which is a claim and not an absence** — #162.
    //
    // Left out at first on the argument that the vocabulary describes how a
    // recording *differs* from the canonical one, which the original by
    // definition does not. The library's own rule says otherwise:
    // `MediumCatalogued.tracks` is an `Option` precisely so "nobody said"
    // stays distinct from "I have them all", and the same distinction holds
    // here — **"carries no remix qualifier" is not "known to be the
    // original"**, and without this they are indistinguishable.
    //
    // Nor is it a rare default. **365 files in this library carry it**, more
    // than `radio-edit`, `extended-mix`, `instrumental` and `live` together:
    // it is the commonest thing a DJ crate says about a track, and "the
    // originals, not the remixes" is a question this library could not ask.
    //
    // **Top level, because it is a variant of nothing.** `partial` sits here
    // for the same reason. And it will never be corroborated — MusicBrainz has
    // no such term — so it rests on `TagVariant` forever, which is what the
    // rungs are for: a claim Beatport made, recorded as Beatport's.
    ("original", None, true),
];

/// Whether this concept may be asserted about a recording.
///
/// A **node label** may not — ISO 25964's term for a concept put into an array
/// purely to organise it, which by the standard's own rule does not take part
/// in retrieval. Refusing is better than accepting and folding to nothing,
/// because an assertion nobody can see is indistinguishable from one nobody
/// made.
pub fn assignable(label: &str) -> bool {
    let wanted = label.trim().to_lowercase();
    SEED.iter()
        .find(|(name, _, _)| *name == wanted)
        .is_none_or(|(_, _, ok)| *ok)
}

/// Spellings the world uses for a seeded concept.
///
/// Only where the variant is a *spelling* rather than a different claim.
/// `a cappella` and `acapella` are the same word badly typed; `radio edit` is
/// not a misspelling of anything here.
const SPELLINGS: &[(&str, &str)] = &[
    ("a cappella", "acappella"),
    ("acapella", "acappella"),
    ("dub plate", "dub-plate"),
    ("dubplate", "dub-plate"),
    // `radio edit` and `radio mix` are one claim; `radio` alone is not, and a
    // segment like `Marcelo trance radio mix` still says it.
    ("radio edit", "radio-edit"),
    ("radio mix", "radio-edit"),
    ("radio version", "radio-edit"),
    ("extended mix", "extended-mix"),
    ("extended version", "extended-mix"),
    // Beatport's phrase, and Traxsource's.
    ("original mix", "original"),
    ("original version", "original"),
];

/// Labels that must match a **whole segment**, never a word inside one.
///
/// `original` is here because a remixer can be called one: `(Original Sinners
/// Remix)` names a duo and says nothing about which version this is, and the
/// word split read it as the original mix. `live` wants the opposite — `Live
/// at Wembley` is a live recording — which is why this is a list and not a
/// rule.
const WHOLE_SEGMENT_ONLY: &[&str] = &["original"];

/// Which version qualifiers a title claims, read from its bracketed parts.
///
/// **Matched against the vocabulary, never against "anything in brackets".**
/// That distinction is the whole rule: `(Nolah Remix)` names *who*, not
/// *what*, and a parser that treated every parenthesis as a qualifier would
/// mint a concept per remixer. The Python side learned this the expensive way
/// — its first guard compared bracketed text literally and downgraded nine
/// good matches out of eleven.
///
/// **`(Radio Edit)` is deliberately not here.** A radio edit is a different
/// length, which makes it a derivation rather than a version of the same
/// recording — the same line the two duration tolerances draw in
/// `gmuse_tools/identify.py`, and it must be drawn the same way on both sides.
///
/// Only the bracketed part is read. A title is free to contain `live` as a
/// word — `Live and Let Die` is not a live recording.
pub fn qualifiers_in(title: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for segment in bracketed(title) {
        let text = normalise_dashes(&segment);
        for (variant, canonical) in SPELLINGS {
            if text.contains(variant) && !found.contains(canonical) {
                found.push(*canonical);
            }
        }
        // The whole segment first, because a seeded label can be hyphenated —
        // `dub-plate` is one concept and splitting on the hyphen would look
        // for two that do not exist. Then each word, so `Live at Wembley`
        // still says live.
        let candidates = std::iter::once(text.as_str()).chain(
            text.split([' ', '-'])
                .filter(|word| !WHOLE_SEGMENT_ONLY.contains(word)),
        );
        for candidate in candidates {
            if let Some((label, _, true)) = SEED.iter().find(|(n, _, _)| *n == candidate)
                && !found.contains(label)
            {
                found.push(*label);
            }
        }
    }
    found
}

/// Role words that reliably introduce a **person** in a bracketed segment.
///
/// Measured against this library rather than guessed, and the measurement is
/// why the obvious ones are absent. Across 6,500 files:
///
/// | word | uses | what the names look like |
/// | --- | --- | --- |
/// | `remix` | 65 | Boards of Canada, Joris Voorn, Benny Benassi — all real |
/// | `mix` | 71 | `1969 Glyn Johns`, `2021`, `Boy Wunder radio` — remaster notes |
/// | `edit` | 13 | `Album`, `radio`, `remix - radio` — version words, not people |
/// | `dub` | 1 | `Uptown` |
///
/// So `mix`, `edit` and `dub` are excluded. They are the words that describe
/// the *version*, which is what `qualifiers_in` reads.
const REMIXER_WORDS: &[&str] = &["remix", "rework", "bootleg", "refix", "vip"];

/// Words that describe a version rather than name a person, so a segment made
/// only of them credits nobody.
///
/// The last three arrived from the tools' own run of this rule over the whole
/// library, which proposed `Vox`, `alternate` and `full` as remixers — from
/// `(Vox Mix)`, `(alternate mix)` and `(full vocal mix)`. `identify.py` keeps
/// the same list, and the two are meant to stay the same list.
const NOT_A_NAME: &[&str] = &[
    "original",
    "radio",
    "extended",
    "club",
    "album",
    "single",
    "vocal",
    "instrumental",
    "dub",
    "main",
    "short",
    "long",
    "remix",
    "vox",
    "alternate",
    "full",
];

/// Who a title says remixed this, where it says so plainly.
///
/// **A guess, and asserted as one.** The name is minted into an artist, which
/// is why the credit carries `Source::TagVariant`: of 76 distinct names this
/// library's titles yield, only 5 are already artists here. The other 71 are
/// real remixers nobody owns an album by — worth having, and worth being able
/// to tell apart from a name MusicBrainz confirmed.
///
/// Conservative on purpose. A segment whose name is only version words
/// (`Original Mix`) or starts with a year (`1969 Glyn Johns Mix`) credits
/// nobody, because those are the shapes that produced every false positive in
/// the measurement above.
pub fn credits_in(title: &str) -> Vec<(&'static str, String)> {
    let mut out: Vec<(&'static str, String)> = Vec::new();
    for segment in bracketed(title) {
        let text = normalise_dashes(&segment);
        let Some(rest) = REMIXER_WORDS.iter().find_map(|w| text.strip_suffix(w)) else {
            continue;
        };
        // Trailing version words are not part of a name: `Hi-Bias Signature
        // radio remix` credits `Hi-Bias Signature`, and keeping the `radio`
        // would mint an artist nobody is called.
        let words: Vec<&str> = rest
            .split_whitespace()
            .rev()
            .skip_while(|w| NOT_A_NAME.contains(w))
            .collect();
        if words.is_empty() || !plausible_name(&words) {
            continue;
        }
        // **Counted in words, never sliced by byte.** `text` has been
        // lowercased, had its dashes folded and its whitespace collapsed, so
        // its byte offsets do not address the title — indexing the original
        // with them lands mid-character on any name carrying a U+2010.
        let as_written = segment
            .split_whitespace()
            .take(words.len())
            .collect::<Vec<_>>()
            .join(" ");
        if !out.iter().any(|(_, n)| n.eq_ignore_ascii_case(&as_written)) {
            out.push(("remixer", as_written));
        }
    }
    out
}

/// Whether a bracketed fragment reads as somebody's name.
/// `words` arrives reversed, so its last element is the name's first word.
fn plausible_name(words: &[&str]) -> bool {
    // `1969 Glyn Johns Mix` and `2021 Mix` are remaster annotations. Fourteen
    // and twelve uses respectively in this library, and the single largest
    // source of false positives in the measurement.
    if words
        .last()
        .is_some_and(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))
    {
        return false;
    }
    words.iter().any(|w| !NOT_A_NAME.contains(w))
}

/// The parenthesised and bracketed parts of a title, in order.
fn bracketed(title: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in title.chars() {
        match c {
            '(' | '[' => {
                depth += 1;
                if depth == 1 {
                    current.clear();
                }
            }
            ')' | ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 && !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            _ if depth > 0 => current.push(c),
            _ => {}
        }
    }
    out
}

/// Lowercased, with every dash the world uses folded to an ASCII hyphen and
/// runs of space collapsed.
///
/// `Pan-Pot` and `Pan‐Pot` differ by one U+2010, and that single character
/// downgraded nine good matches on the Python side before anybody noticed.
fn normalise_dashes(text: &str) -> String {
    let folded: String = text
        .chars()
        .map(|c| match c {
            '\u{2010}'..='\u{2015}' | '\u{2212}' | '\u{ff0d}' => '-',
            c => c,
        })
        .collect();
    crate::model::ingest::normalise(&folded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::entity::Concept;
    use crate::model::facet;
    use crate::model::id::Id;

    /// A grouping concept is a place in the tree, not a claim about a song.
    #[test]
    fn a_grouping_concept_cannot_be_asserted() {
        assert!(!assignable("mix-variant"));
        assert!(!assignable("performance-variant"));
        assert!(assignable("instrumental"));
        assert!(assignable("live"));
        assert!(assignable("partial"), "a leaf at top level is still a leaf");
    }

    /// A concept nobody seeded is assumed assignable: the scheme grows by
    /// fact, so refusing what is not in the seed would freeze the vocabulary.
    #[test]
    fn an_unseeded_concept_is_assignable() {
        assert!(assignable("karaoke"));
    }

    /// The claim a title makes, when it makes one.
    #[test]
    fn a_bracketed_version_word_is_read() {
        assert_eq!(qualifiers_in("212 (Instrumental)"), ["instrumental"]);
        assert_eq!(qualifiers_in("Years [Live]"), ["live"], "brackets too");
        assert_eq!(
            qualifiers_in("Rebellion (Live at Wembley)"),
            ["live"],
            "the qualifier is a word in the segment, not the whole of it"
        );
        assert_eq!(
            qualifiers_in("Halcyon (Live) (Instrumental)"),
            ["live", "instrumental"],
            "a recording can be two things at once"
        );
    }

    /// **A remixer's name is not a qualifier.** The rule the whole parser is
    /// built around: matched against the vocabulary, never against "anything
    /// in brackets". A parser that took every parenthesis would mint a
    /// concept per remixer, and the Python side's first attempt at this
    /// downgraded nine good matches out of eleven.
    #[test]
    fn a_remixers_name_is_not_read_as_a_version() {
        assert!(qualifiers_in("Phobos (Pan-Pot Remix)").is_empty());
        assert!(qualifiers_in("Modulo Tierra (Nolah Remix)").is_empty());
        assert!(
            qualifiers_in("Slip Away (Larse Vox Remix)").is_empty(),
            "`vox` is not `acappella`, and a name is not a vocabulary"
        );
    }

    /// **A length variant is a version too.**
    ///
    /// This test asserted the opposite, on the grounds that a radio edit is a
    /// derivation rather than a version. That conflated two jobs: the duration
    /// tolerances decide whether two things are the *same recording*, and this
    /// vocabulary describes *what you are holding*. Excluding it made "no
    /// radio edits in this playlist" unaskable about files that plainly say
    /// they are radio edits.
    #[test]
    fn a_length_variant_is_a_version() {
        assert_eq!(qualifiers_in("Alors on danse (Radio Edit)"), ["radio-edit"]);
        assert_eq!(qualifiers_in("Sandstorm (Extended Mix)"), ["extended-mix"]);
        assert_eq!(
            qualifiers_in("Blue Monday (Radio Version)"),
            ["radio-edit"],
            "one claim, however it is worded"
        );
    }

    /// **"This is the original" is a claim, not an absence** — #162.
    ///
    /// This asserted the opposite, on the argument that the vocabulary
    /// describes how a recording *differs* from the canonical one and the
    /// original by definition does not. The library's own rule overturned it:
    /// `MediumCatalogued.tracks` is an `Option` precisely so "nobody said"
    /// stays distinct from "I have them all", and **"carries no remix
    /// qualifier" is not "known to be the original"**.
    ///
    /// Nor is it rare. 365 files here carry it — more than `radio-edit`,
    /// `extended-mix`, `instrumental` and `live` together — and "the
    /// originals, not the remixes" was a question this library could not ask.
    #[test]
    fn an_original_mix_says_so() {
        assert_eq!(qualifiers_in("Distant Trip (Original Mix)"), ["original"]);
        assert_eq!(
            qualifiers_in("Cream (original version)"),
            ["original"],
            "one claim, however it is worded"
        );
        // And it is still a *version* claim and never a remixer: the segment
        // has to say `original`, not merely contain a name that does.
        assert!(qualifiers_in("Phobos (Original Sinners Remix)").is_empty());
    }

    /// **The name in a title is evidence, and it says who.**
    ///
    /// It used to be discarded: `qualifiers_in` correctly refused to treat
    /// `(Nolah Remix)` as a version, and nothing else looked at it, so the
    /// file was telling us a credit nobody wrote down.
    #[test]
    fn a_title_names_the_person_who_remixed_it() {
        assert_eq!(
            credits_in("Modulo Tierra (Nolah Remix)"),
            [("remixer", "Nolah".to_string())]
        );
        assert_eq!(
            credits_in("Phobos (Pan-Pot Remix)"),
            [("remixer", "Pan-Pot".to_string())],
            "the credited spelling comes from the title, not from the fold"
        );
        assert_eq!(
            credits_in("Sunrise (Boards of Canada Rework)"),
            [("remixer", "Boards of Canada".to_string())]
        );
    }

    /// A trailing version word is not part of a name, and a multi-byte one is
    /// not a place to slice.
    ///
    /// The credited spelling used to be cut out of the title by byte offset,
    /// taken from a string that had already been lowercased, dash-folded and
    /// whitespace-collapsed. Any name carrying a U+2010 would have landed
    /// mid-character. It is counted in words now.
    #[test]
    fn the_credited_spelling_survives_the_normalisation() {
        assert_eq!(
            credits_in("Someday (Hi-Bias Signature radio remix)"),
            [("remixer", "Hi-Bias Signature".to_string())]
        );
        assert_eq!(
            credits_in("The Alchemist (Roman Flügel Remix)"),
            [("remixer", "Roman Flügel".to_string())],
            "the name keeps its accents and its case"
        );
        assert_eq!(
            credits_in("Phobos (Pan‐Pot Remix)"),
            [("remixer", "Pan‐Pot".to_string())],
            "a U+2010 matches, and is not sliced through"
        );
    }

    /// One title can say two different things, and each goes to its own fact.
    /// `Initiation (Orbital remix) (radio edit)` is a real row here.
    #[test]
    fn a_title_can_name_who_and_say_which_version() {
        let title = "Initiation (Orbital remix) (radio edit)";
        assert_eq!(credits_in(title), [("remixer", "Orbital".to_string())]);
        assert_eq!(qualifiers_in(title), ["radio-edit"]);
    }

    /// **The false positives the measurement found, each one refused.**
    ///
    /// Across this library's 6,500 files, `mix` matched 71 segments and `edit`
    /// 13, and almost none of them named a person: `1969 Glyn Johns Mix` and
    /// `2021 Mix` are remaster annotations, `Album Edit` and `radio edit`
    /// describe the version. Those two words are why the role list is five
    /// entries rather than the eight that first looked obvious.
    #[test]
    fn a_version_word_is_not_a_persons_name() {
        for title in [
            "Get Back (1969 Glyn Johns Mix)",
            "Something (2021 Mix)",
            // Through `remix`, so the year guard is what refuses these rather
            // than the role-word list. Without one they credit an artist
            // called `2021`.
            "Something (2021 Remix)",
            "Get Back (1969 Glyn Johns Remix)",
            "Blue Monday (Album Edit)",
            "Alors on danse (radio edit)",
            "Distant Trip (Original Mix)",
            "Uptown (Uptown Dub)",
        ] {
            assert!(credits_in(title).is_empty(), "{title} credited somebody");
        }
    }

    /// Only the bracketed part is read, or every song with `live` in its name
    /// becomes a live recording.
    #[test]
    fn an_unbracketed_word_claims_nothing() {
        assert!(qualifiers_in("Live and Let Die").is_empty());
        assert!(qualifiers_in("Alive").is_empty());
        assert!(qualifiers_in("Demolition").is_empty(), "not `demo`");
    }

    /// The spellings the world actually uses, and the dash it uses to write
    /// them — `Pan-Pot` against `Pan‐Pot` differs by one U+2010, and that one
    /// character cost nine matches before anybody saw it.
    #[test]
    fn a_badly_typed_spelling_is_still_the_word() {
        for title in ["S (A Cappella)", "S (Acapella)", "S (acappella)"] {
            assert_eq!(qualifiers_in(title), ["acappella"], "{title}");
        }
        for title in ["S (Dub Plate)", "S (Dub‐Plate)", "S (dubplate)"] {
            assert_eq!(qualifiers_in(title), ["dub-plate"], "{title}");
        }
    }

    /// A grouping concept cannot be claimed by a title either — it is not
    /// assignable, so the parser must not offer it.
    #[test]
    fn a_title_cannot_claim_a_grouping_concept() {
        assert!(qualifiers_in("Whatever (Mix Variant)").is_empty());
    }

    /// Convergent, and case-folded, exactly as genre is — one minting for
    /// every axis since #117, so two vocabularies cannot drift apart.
    #[test]
    fn one_label_one_concept() {
        assert_eq!(
            Id::<Concept>::in_facet(facet::VERSION, "Instrumental"),
            Id::<Concept>::in_facet(facet::VERSION, " instrumental ")
        );
        assert_ne!(
            Id::<Concept>::in_facet(facet::VERSION, "live"),
            Id::<Concept>::in_facet(facet::VERSION, "demo")
        );
    }

    /// Every seeded parent is itself seeded, or the closure has a dangling
    /// edge the moment the scheme is written to the log.
    #[test]
    fn every_parent_is_a_concept() {
        let names: Vec<&str> = SEED.iter().map(|(n, _, _)| *n).collect();
        for (name, parent, _) in SEED {
            if let Some(p) = parent {
                assert!(
                    names.contains(p),
                    "{name} names a parent that is not seeded: {p}"
                );
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod fold_tests {
    use crate::model::entity::{Concept, Device, Recording};
    use crate::model::facet::{self, Described};
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;

    pub(crate) fn device(n: u128) -> Id<Device> {
        Id::from_uuid(uuid::Uuid::from_u128(n))
    }

    pub(crate) fn rec() -> Id<Recording> {
        Id::from_uuid(uuid::Uuid::from_u128(212))
    }

    /// Does anybody say so — the question the player asks, which does not
    /// care who. The review queue asks the other one, by reading the rows.
    pub(crate) fn live(i: &Instance, label: &str) -> bool {
        let concept = Id::<Concept>::in_facet(facet::VERSION, label);
        let cat = i.catalogue();
        // **Not shadowed**, which is a different question from "was claimed".
        // A retracted pair keeps its claims — dropping them would destroy the
        // provenance saying where they came from (#96) — so the retraction
        // has to be consulted rather than inferred from an empty map.
        !cat.assertions_retracted
            .contains_key(&(Described::Recording(rec()), concept))
            && cat
                .assertions
                .keys()
                .any(|(r, c, _)| *r == Described::Recording(rec()) && *c == concept)
    }

    /// Every asserter of one claim, with what each of them settled on.
    pub(crate) fn claims(i: &Instance, label: &str) -> Vec<(Source, Confidence)> {
        let concept = Id::<Concept>::in_facet(facet::VERSION, label);
        i.catalogue()
            .assertions
            .iter()
            .filter(|((r, c, _), _)| *r == Described::Recording(rec()) && *c == concept)
            .map(|((_, _, basis), conf)| (*basis, *conf))
            .collect()
    }

    /// A recording can be two things at once, which a field could not say.
    #[test]
    fn a_recording_carries_several_qualifiers() {
        let mut i = Instance::new(device(1));
        i.assign_version(
            100,
            rec(),
            "live",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        i.assign_version(
            200,
            rec(),
            "instrumental",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        assert!(live(&i, "live") && live(&i, "instrumental"));
    }

    /// **The correction survives the sync**, which is the whole reason a
    /// retraction is a fact rather than a delete. An import asserts, a person
    /// disagrees, and a stale replica re-delivering the import cannot
    /// resurrect it — the retraction carries the later stamp.
    #[test]
    fn a_retraction_is_not_undone_by_a_replica() {
        let mut mine = Instance::new(device(1));
        mine.assign_version(
            100,
            rec(),
            "instrumental",
            Source::Discogs,
            Confidence::Matched,
        );
        let import = mine.records().clone();

        mine.retract_version(200, rec(), "instrumental", Source::Manual);
        assert!(!live(&mine, "instrumental"), "the retraction did not take");

        // The other machine still holds only the import and sends it back.
        mine.merge(300, &import);
        assert!(
            !live(&mine, "instrumental"),
            "a stale replica resurrected a retracted qualifier"
        );
    }

    /// **A person's correction survives the next import** — #96.
    ///
    /// This is what "an override with precedence by basis" buys, and without
    /// it the edit is worthless: retracting a wrong Discogs claim lasts
    /// exactly until the next Discogs pass, and an edit that does not survive
    /// a resync is not an edit.
    ///
    /// The import is still **recorded**. Dropping it would destroy the
    /// provenance saying where it came from, which is the thing that makes an
    /// override inspectable rather than magic — it is shadowed, not deleted.
    #[test]
    fn an_import_does_not_undo_a_persons_correction() {
        let mut i = Instance::new(device(1));
        i.assign_version(100, rec(), "live", Source::Discogs, Confidence::Matched);
        assert!(live(&i, "live"));

        // A person disagrees.
        i.retract_version(200, rec(), "live", Source::Manual);
        assert!(!live(&i, "live"), "the correction did not take");

        // Discogs says it again, later and just as confidently.
        i.assign_version(300, rec(), "live", Source::Discogs, Confidence::Matched);
        assert!(
            !live(&i, "live"),
            "an import undid a person's correction — the edit did not survive a resync"
        );

        // And the import is on the record, so why it keeps being claimed is
        // answerable rather than mysterious.
        assert_eq!(
            claims(&i, "live"),
            [(Source::Discogs, Confidence::Matched)],
            "the shadowed claim lost its provenance"
        );

        // The person may change their mind, because their own voice is not
        // weaker than the one that ruled it out.
        i.assign_version(400, rec(), "live", Source::Manual, Confidence::Confirmed);
        assert!(
            live(&i, "live"),
            "a person could not undo their own retraction"
        );
    }

    /// And the weaker voice cannot overrule the stronger, in either direction.
    #[test]
    fn a_tag_cannot_undo_what_a_person_ruled_out() {
        let mut i = Instance::new(device(1));
        i.retract_version(100, rec(), "live", Source::Manual);
        i.assign_version(
            200,
            rec(),
            "live",
            Source::TagVariant,
            Confidence::Provisional,
        );
        assert!(!live(&i, "live"), "a tag overruled a person");

        // But a person's assertion beats a tag's retraction, which is the
        // same rule read the other way.
        let mut j = Instance::new(device(1));
        j.retract_version(100, rec(), "live", Source::TagVariant);
        j.assign_version(200, rec(), "live", Source::Manual, Confidence::Confirmed);
        assert!(live(&j, "live"), "a person could not overrule a tag");
    }

    /// **"Decided not to apply" is not "nobody said", and the fold has to keep
    /// them apart.**
    ///
    /// A retraction leaves the pair present and empty rather than removing it.
    /// Both spellings fold to the same answer for `is this live?`, which is why
    /// a test asking only that passed against either — but the query language
    /// needs three states, not two: `qualifier = live`, `not qualifier = live`,
    /// and `not qualifier?` for the review queue. Only a row that exists and is
    /// empty can say the middle one.
    #[test]
    fn a_retraction_is_a_decision_and_not_a_silence() {
        let mut i = Instance::new(device(1));
        i.assign_version(100, rec(), "live", Source::Discogs, Confidence::Matched);
        i.retract_version(200, rec(), "live", Source::Manual);

        let cat = i.catalogue();
        assert!(
            cat.assertions_retracted.contains_key(&(
                Described::Recording(rec()),
                Id::<Concept>::in_facet(facet::VERSION, "live")
            )),
            "a retraction has to leave a row saying it was decided — dropping \
             it makes a decision indistinguishable from a silence"
        );
        assert!(
            !cat.assertions_retracted.contains_key(&(
                Described::Recording(rec()),
                Id::<Concept>::in_facet(facet::VERSION, "instrumental")
            )),
            "and something nobody has ruled on has no row at all"
        );
    }

    /// And a later assertion still wins, or a retraction would be permanent —
    /// which is a delete wearing a different name.
    #[test]
    fn a_later_assertion_undoes_a_retraction() {
        let mut i = Instance::new(device(1));
        i.assign_version(100, rec(), "live", Source::Discogs, Confidence::Matched);
        i.retract_version(200, rec(), "live", Source::Manual);
        i.assign_version(300, rec(), "live", Source::Manual, Confidence::Confirmed);
        assert!(live(&i, "live"));
    }

    /// The import stays in the log after a retraction. Deleting it would
    /// destroy the provenance that says where the claim came from.
    #[test]
    fn a_retraction_keeps_what_it_shadows() {
        let mut i = Instance::new(device(1));
        i.assign_version(
            100,
            rec(),
            "instrumental",
            Source::Discogs,
            Confidence::Matched,
        );
        i.retract_version(200, rec(), "instrumental", Source::Manual);
        let kinds: Vec<_> = i
            .records()
            .iter()
            .filter(|r| format!("{:?}", r.fact).starts_with("ConceptAsserted"))
            .collect();
        assert_eq!(kinds.len(), 1, "the shadowed assertion was thrown away");
    }

    /// A grouping concept is refused rather than accepted and folded to
    /// nothing, because an assignment nobody can see reads as one nobody made.
    #[test]
    fn a_grouping_concept_is_refused_at_the_door() {
        let mut i = Instance::new(device(1));
        let taken = i.assign_version(
            100,
            rec(),
            "mix-variant",
            Source::Manual,
            Confidence::Confirmed,
        );
        assert!(!taken);
        assert!(i.records().is_empty(), "a refused assignment still wrote");
    }

    /// Seeding twice writes the scheme once: every id derives from its label.
    #[test]
    fn seeding_is_idempotent() {
        let mut i = Instance::new(device(1));
        i.seed_versions(100);
        let after_one = i.catalogue().concepts.len();
        i.seed_versions(200);
        assert_eq!(i.catalogue().concepts.len(), after_one);
        assert!(i.catalogue().narrows.len() >= 6);
    }
}

#[cfg(test)]
mod claim_tests {
    //! **Claim identity versus claim attribute**, the two tiers an assignment
    //! folds by: `(subject, concept, basis)` is what makes a row exist and it
    //! unions; `confidence` rides on it and the latest stamp wins.
    //!
    //! The rule exists because the alternative loses the most useful thing two
    //! sources can say, which is that they agree. Genre folds identically and
    //! is tested through the round trip.

    use super::fold_tests::{claims, device, live, rec};
    use crate::model::entity::Concept;
    use crate::model::facet::{self, Described};
    use crate::model::id::Id;
    use crate::model::ingest::Confidence;
    use crate::model::instance::Instance;
    use crate::model::reference::Source;

    fn assign(i: &mut Instance, at: i64, basis: Source, confidence: Confidence) {
        i.assign_version(at, rec(), "instrumental", basis, confidence);
    }

    /// **Case 1 — the same source, re-imported.** Idempotent by construction,
    /// which is what lets a pass be re-run without reviewing its output twice.
    #[test]
    fn re_importing_a_source_does_not_make_a_second_claim() {
        let mut i = Instance::new(device(1));
        assign(&mut i, 100, Source::Discogs, Confidence::Matched);
        assign(&mut i, 200, Source::Discogs, Confidence::Matched);
        assert_eq!(claims(&i, "instrumental").len(), 1);
    }

    /// **Case 2 — the same source, re-scored.** The attribute moves, and the
    /// log keeps both records: the fold is a view, never the history.
    #[test]
    fn a_source_that_changes_its_mind_updates_in_place() {
        let mut i = Instance::new(device(1));
        assign(&mut i, 100, Source::Discogs, Confidence::Provisional);
        assign(&mut i, 200, Source::Discogs, Confidence::Matched);
        assert_eq!(
            claims(&i, "instrumental"),
            vec![(Source::Discogs, Confidence::Matched)]
        );
        assert_eq!(
            i.records().len(),
            4,
            "the earlier score was dropped from the log rather than shadowed"
        );
    }

    /// **Case 3 — two sources, one concept, and the reason for the whole
    /// rule.** Under the old key the human's claim overwrote Discogs's and the
    /// agreement vanished, leaving nothing to say a guess had been confirmed.
    #[test]
    fn two_asserters_of_one_concept_both_stay() {
        let mut i = Instance::new(device(1));
        assign(&mut i, 100, Source::Discogs, Confidence::Matched);
        assign(&mut i, 200, Source::Manual, Confidence::Confirmed);
        assert_eq!(
            claims(&i, "instrumental"),
            // Ordered by basis, which `Source` declares weakest-first — so a
            // review surface reading this map gets the least authoritative
            // claim first without sorting.
            vec![
                (Source::Discogs, Confidence::Matched),
                (Source::Manual, Confidence::Confirmed),
            ],
            "one asserter overwrote the other, and the agreement was the answer"
        );
    }

    /// **Case 4 — nothing edits somebody else's claim.** A person promoting an
    /// import is making their own, which is why case 3's row count is two: the
    /// import still says only what the import said.
    #[test]
    fn a_person_agreeing_does_not_rewrite_the_import() {
        let mut i = Instance::new(device(1));
        assign(&mut i, 100, Source::Discogs, Confidence::Provisional);
        assign(&mut i, 200, Source::Manual, Confidence::Confirmed);
        assert!(
            claims(&i, "instrumental").contains(&(Source::Discogs, Confidence::Provisional)),
            "the human's confidence was written onto the import's row"
        );
    }

    /// **Case 5 — a retraction is keyed one field shorter.** It answers the
    /// question rather than arguing with an asserter, so it silences every
    /// basis; otherwise a person would have to retract once per source that
    /// ever said it, and the next import would put it back.
    #[test]
    fn one_retraction_silences_every_asserter() {
        let mut i = Instance::new(device(1));
        assign(&mut i, 100, Source::Discogs, Confidence::Matched);
        assign(&mut i, 200, Source::TagVariant, Confidence::Provisional);
        i.retract_version(300, rec(), "instrumental", Source::Manual);
        assert!(claims(&i, "instrumental").is_empty());

        // And any basis revives it, because a retraction is a decision and not
        // a tombstone — the same reason it is a fact rather than a delete.
        assign(&mut i, 400, Source::Manual, Confidence::Confirmed);
        assert!(live(&i, "instrumental"));
        assert!(
            !i.catalogue().assertions_retracted.contains_key(&(
                Described::Recording(rec()),
                Id::<Concept>::in_facet(facet::VERSION, "instrumental")
            )),
            "the pair still reads as ruled against while a claim stands"
        );
    }
}
