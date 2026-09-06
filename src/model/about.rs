//! What the library says about one thing — issue #125, `info-panel.md` §2–§3.
//!
//! One question, [`of`], answering with what to draw: a title, and sections of
//! rows. Everything after this in the milestone is drawing and navigating.
//!
//! ## The row kinds are the edge kinds
//!
//! `info-panel.md` §2 says this model has three kinds of edge and they want
//! different treatment, so the distinction lives in the type rather than in
//! how the drawing code happens to format things:
//!
//! - a **[`Row::Link`]** is structural. A track is on a medium; nobody
//!   asserted that, it is a fact about a pressing. Following one is a move.
//! - a **[`Row::Claim`]** is descriptive, and carries **who said it and how
//!   sure**. The catalogue keys assertions by `(subject, concept, source)`
//!   precisely so two sources stay two rows, and #96 made a retraction shadow
//!   rather than delete — so a ruled-out claim is *present and marked*, never
//!   missing. "Somebody decided against this" and "nobody ever said it" are
//!   different answers.
//! - a **[`Row::Fact`]** is what the fold simply is. A bitrate has no asserter.
//!
//! [`Row::Many`] is a fourth and is not a fourth edge kind: it is a fan-out
//! **summarised**. An artist with four hundred recordings cannot list them, so
//! it says how many and hands over the query that finds them — which makes the
//! panel a way *into* the query language rather than a competitor to it.
//!
//! ## Scanning is right here
//!
//! #121 and #123 were both the same mistake: a linear scan, correct for one
//! question, run once per row. This is the other case. `of` answers about
//! **one focus**, once, when a panel is opened — so walking the credits or the
//! entries is the cheap way and building an index would be the expensive one.
//! Measured rather than assumed: see `perf` at the end of this file.

#![allow(
    dead_code,
    reason = "the model is tested ahead of its callers; exercised by tests, no production caller yet — #126 is the panel that draws this, and answering the question is worth landing before the thing that asks it"
)]

use crate::model::entity::{Collection, Concept, Encoding, Facet, Recording};
use crate::model::facet::Described;
use crate::model::focus::Focus;
use crate::model::id::Id;
use crate::model::ingest::Confidence;
use crate::model::reference::{CreditOn, Source};
use crate::model::seam::Librarian;

/// Everything the panel draws for one focus.
#[derive(Debug, Clone, PartialEq)]
pub struct About {
    /// The word for what this is — `Focus::kind`.
    pub kind: &'static str,
    pub title: String,
    pub subtitle: Option<String>,
    pub sections: Vec<Section>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Section {
    pub title: &'static str,
    pub rows: Vec<Row>,
}

/// One line of a section.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    /// A structural edge, with somewhere to go.
    Link {
        label: String,
        name: String,
        to: Focus,
    },
    /// A claim, and who made it.
    ///
    /// `retracted` is the basis that ruled it out, when something did — the
    /// row stays, struck. `to` is present when the claimed thing has a page of
    /// its own, which a concept does.
    Claim {
        label: String,
        value: String,
        /// Who said it and how sure. `None` when the assertion has been swept
        /// and only the retraction is left — see `seam::Claimed`.
        said: Option<(Source, Confidence)>,
        retracted: Option<Source>,
        to: Option<Focus>,
    },
    /// What the fold is. No asserter, nowhere to go.
    Fact { label: String, value: String },
    /// Too many to list: how many, and the query that finds them.
    Many {
        label: String,
        count: usize,
        query: String,
    },
    /// **A recorded fact** — the third edge kind, #128.
    ///
    /// A claim's basis says *who*; this says *when* and *on which machine*,
    /// and a withdrawal says somebody changed their mind.
    Happened {
        at: i64,
        device: String,
        name: String,
        detail: String,
        withdrawal: bool,
    },
}

/// The section the fact log goes in.
///
/// **Named as a constant** because the panel folds it away by default and has
/// to know which one it is — two spellings of one section title is how a fold
/// silently stops working.
pub const FACTS: &str = "facts";

/// Beyond this a section says how many rather than listing them.
///
/// **Not a rendering limit.** A panel that scrolled four hundred recordings
/// would be a worse library view than the library view, and the query it hands
/// over is the thing that is actually good at showing them.
const AT_MOST: usize = 24;

/// What the library says about a focus.
///
/// `history` asks for the fact log too. **Off by default and asked for**,
/// because it is the expensive part — a filter over every record, 3.6ms on
/// this library against 66µs for the rest — and the panel folds it away until
/// somebody opens it. Building what is hidden, on every frame, is how a static
/// screen comes to cost 8% of a core.
pub fn of(l: &Librarian, focus: Focus, history: bool) -> About {
    let mut about = About {
        kind: focus.kind(),
        title: label_of(l, focus),
        subtitle: None,
        sections: Vec::new(),
    };
    match focus {
        Focus::Recording(id) => recording(l, id, &mut about),
        Focus::Encoding(id) => encoding(l, id, &mut about),
        Focus::ReleaseGroup(id) => release_group(l, id, &mut about),
        Focus::Release(id) => release(l, id, &mut about),
        Focus::Artist(id) => artist(l, id, &mut about),
        Focus::Concept(id) => concept(l, id, &mut about),
        Focus::Collection(id) => collection(l, id, &mut about),
        Focus::Work(id) => work(l, id, &mut about),
    }
    // **Every focus gets its history**, not only a recording: a concept
    // somebody catalogued and a crate somebody named were both decided, and
    // when is a fair question about either.
    let facts: Vec<Row> = if history {
        l.facts_about(focus)
    } else {
        Vec::new()
    }
    .into_iter()
    .map(|h| Row::Happened {
        at: h.at,
        device: h.device,
        name: h.name,
        detail: h.detail,
        withdrawal: h.withdrawal,
    })
    .collect();
    about.sections.retain(|s| !s.rows.is_empty());
    // **The header stays even with nothing under it**, unlike every other
    // section: it is folded by default, and a heading that vanished when shut
    // would leave no way to open it.
    about.sections.push(section(FACTS, facts));
    about
}

/// What to call a focus on screen.
///
/// **Here rather than on `Focus`** (#124): naming a recording needs the spine
/// and naming a concept needs the vocabulary, so a `Focus` that could name
/// itself would have to carry a library around.
pub fn label_of(l: &Librarian, focus: Focus) -> String {
    let unknown = || "(not in the library)".to_string();
    match focus {
        Focus::Recording(id) => l
            .spine
            .recordings
            .get(&id)
            .map_or_else(unknown, |r| r.title.clone()),
        Focus::Encoding(id) => l
            .encodings
            .encodings
            .get(&id)
            .map_or_else(unknown, |e| format!("{:?} · {} kbps", e.format, e.bitrate)),
        Focus::ReleaseGroup(id) => l
            .spine
            .groups
            .get(&id)
            .map_or_else(unknown, |g| g.title.clone()),
        Focus::Release(id) => l
            .spine
            .releases
            .get(&id)
            .map_or_else(unknown, |r| r.title.clone()),
        Focus::Artist(id) => l
            .reference
            .artists
            .get(&id)
            .map_or_else(unknown, |a| a.name.clone()),
        Focus::Concept(id) => l
            .vocabulary
            .genres
            .get(&id)
            .map_or_else(unknown, |c| c.name.clone()),
        Focus::Collection(id) => l
            .collections
            .get(&id)
            .map_or_else(unknown, |c| c.name.clone()),
        Focus::Work(id) => l
            .spine
            .works
            .get(&id)
            .map_or_else(unknown, |w| w.title.clone()),
    }
}

/// A value as the filter language would quote it, so a `Many` row hands over
/// something that parses.
fn quoted(value: &str) -> String {
    format!("{:?}", value)
}

fn section(title: &'static str, rows: Vec<Row>) -> Section {
    Section { title, rows }
}

fn fact(label: &str, value: impl Into<String>) -> Row {
    Row::Fact {
        label: label.to_string(),
        value: value.into(),
    }
}

fn recording(l: &Librarian, id: Id<Recording>, about: &mut About) {
    about.subtitle =
        Some(l.reference.credited_as(CreditOn::Recording(id))).filter(|s| !s.is_empty());

    // What it is: the identity, and whether anything was folded into it.
    let mut is = Vec::new();
    if let Some(basis) = l.identified_by(id) {
        is.push(fact("identity", basis));
    }
    for was in l.merged_into(id) {
        is.push(fact("merged from", was.to_string()));
    }
    about.sections.push(section("what it is", is));

    about.sections.push(section(
        "said about it",
        claims(l, Described::Recording(id)),
    ));

    let credits = l
        .reference
        .credits_on(CreditOn::Recording(id))
        .into_iter()
        .map(|c| Row::Link {
            label: l.role_of(c.role.id()),
            name: c
                .as_credited
                .clone()
                .unwrap_or_else(|| label_of(l, Focus::Artist(c.artist.id()))),
            to: Focus::Artist(c.artist.id()),
        })
        .collect();
    about.sections.push(section("credited to", credits));

    // Where it sits: the spine, upward.
    let mut sits = Vec::new();
    if let Some(work) = l
        .spine
        .recordings
        .get(&id)
        .and_then(|r| r.work)
        .map(|w| w.id())
    {
        sits.push(Row::Link {
            label: "performs".into(),
            name: label_of(l, Focus::Work(work)),
            to: Focus::Work(work),
        });
    }
    for placed in l.slots_of(id) {
        if let Some(medium) = l.spine.media.get(&placed.medium.id())
            && let Some(rel) = l.spine.releases.get(&medium.release.id())
        {
            sits.push(Row::Link {
                label: "on release".into(),
                name: rel.title.clone(),
                to: Focus::Release(rel.id),
            });
            sits.push(Row::Link {
                label: "in group".into(),
                name: label_of(l, Focus::ReleaseGroup(rel.group.id())),
                to: Focus::ReleaseGroup(rel.group.id()),
            });
            sits.push(fact(
                "track",
                format!("{} on medium {}", placed.position, medium.position),
            ));
        }
    }
    about.sections.push(section("where it sits", sits));

    let copies = l
        .encodings
        .of(id)
        .into_iter()
        .map(|e| Row::Link {
            // **Where, not what** — the label column says which machine, and
            // the name says what the bytes are. The path is a fact about one
            // device and lives on the copy's own page, one keystroke away,
            // rather than filling this column with an absolute path.
            label: if l.where_held(e.id).is_some() {
                "here".to_string()
            } else {
                "elsewhere".to_string()
            },
            name: label_of(l, Focus::Encoding(e.id)),
            to: Focus::Encoding(e.id),
        })
        .collect();
    about.sections.push(section("copies", copies));

    let mut history = Vec::new();
    match l.plays.get(&id).copied().unwrap_or(0) {
        0 => history.push(fact("played", "never")),
        n => history.push(fact("played", format!("{n}×"))),
    }
    if let Some(stars) = l.ratings.get(&id) {
        history.push(fact("rated", format!("{stars}")));
    }
    about.sections.push(section("history", history));

    let crates = l
        .crates_holding(id)
        .into_iter()
        .map(|c| Row::Link {
            label: "in".into(),
            name: label_of(l, Focus::Collection(c)),
            to: Focus::Collection(c),
        })
        .collect();
    about.sections.push(section("filed in", crates));
}

/// Every claim on every axis, retracted ones included and marked.
fn claims(l: &Librarian, subject: Described) -> Vec<Row> {
    let mut rows = Vec::new();
    for (key, facet) in crate::model::facet::ALL {
        let id = Id::<Facet>::of_key(key);
        for claim in l.claims_on(subject, id) {
            rows.push(Row::Claim {
                label: (*facet).to_string(),
                value: claim.label.clone(),
                said: claim.said,
                retracted: claim.retracted,
                to: Some(Focus::Concept(Id::<Concept>::in_facet(key, &claim.label))),
            });
        }
    }
    rows
}

fn encoding(l: &Librarian, id: Id<Encoding>, about: &mut About) {
    let Some(e) = l.encodings.encodings.get(&id) else {
        return;
    };
    about.subtitle = l.where_held(id);

    let mut is = vec![
        fact("length", format!("{}s", e.length.as_secs())),
        fact("format", format!("{:?}", e.format)),
        fact("bitrate", format!("{} kbps", e.bitrate)),
    ];
    for (label, value) in [
        ("sample rate", e.sample_rate.map(|r| format!("{r} Hz"))),
        ("channels", e.channels.map(|c| format!("{c}"))),
        ("cutoff", e.cutoff_hz.map(|m| format!("{} Hz", m.value))),
    ] {
        if let Some(value) = value {
            is.push(fact(label, value));
        }
    }
    is.push(fact("verdict", format!("{:?}", e.verdict())));
    about.sections.push(section("what it is", is));

    about.sections.push(section(
        "held",
        l.holdings_of(id)
            .into_iter()
            .map(|(device, at)| Row::Fact {
                label: device,
                value: at,
            })
            .collect(),
    ));

    if let Some(recording) = e.recording.map(|r| r.id()) {
        about.sections.push(section(
            "of",
            vec![Row::Link {
                label: "recording".into(),
                name: label_of(l, Focus::Recording(recording)),
                to: Focus::Recording(recording),
            }],
        ));
    }
}

fn release_group(l: &Librarian, id: Id<crate::model::entity::ReleaseGroup>, about: &mut About) {
    about.subtitle =
        Some(l.reference.credited_as(CreditOn::ReleaseGroup(id))).filter(|s| !s.is_empty());
    about.sections.push(section(
        "said about it",
        claims(l, Described::ReleaseGroup(id)),
    ));

    let releases: Vec<Row> = l
        .spine
        .releases
        .values()
        .filter(|r| r.group.id() == id)
        .map(|r| Row::Link {
            label: "release".into(),
            name: r.title.clone(),
            to: Focus::Release(r.id),
        })
        .collect();
    about.sections.push(section("editions", releases));
}

fn release(l: &Librarian, id: Id<crate::model::entity::Release>, about: &mut About) {
    let Some(r) = l.spine.releases.get(&id) else {
        return;
    };
    let mut is = Vec::new();
    if let Some(date) = r.released {
        is.push(fact("released", format!("{date:?}")));
    }
    if let Some(label) = r.label
        && let Some(named) = l.reference.labels.get(&label.id())
    {
        is.push(fact("label", named.name.clone()));
    }
    about.sections.push(section("what it is", is));
    about.sections.push(section(
        "in",
        vec![Row::Link {
            label: "release group".into(),
            name: label_of(l, Focus::ReleaseGroup(r.group.id())),
            to: Focus::ReleaseGroup(r.group.id()),
        }],
    ));
}

fn artist(l: &Librarian, id: Id<crate::model::entity::Artist>, about: &mut About) {
    let name = label_of(l, Focus::Artist(id));
    let credited = l
        .reference
        .credits
        .iter()
        .filter(|c| c.artist.id() == id)
        .count();
    about.sections.push(section(
        "credited on",
        vec![Row::Many {
            label: "things".into(),
            count: credited,
            query: format!("artist={}", quoted(&name)),
        }],
    ));
}

fn concept(l: &Librarian, id: Id<Concept>, about: &mut About) {
    let name = label_of(l, Focus::Concept(id));
    let mut is = Vec::new();
    for (key, facet) in crate::model::facet::ALL {
        if Id::<Concept>::in_facet(key, &name) == id {
            is.push(fact("axis", (*facet).to_string()));
            about.sections.push(section("what it is", is));
            about.sections.push(section(
                "asserted of",
                vec![Row::Many {
                    label: "rows".into(),
                    count: l.asserting(id),
                    query: format!("{key}={}", quoted(&name)),
                }],
            ));
            break;
        }
    }

    let up: Vec<Row> = l
        .broader_than(id)
        .into_iter()
        .map(|parent| Row::Link {
            label: "narrower than".into(),
            name: label_of(l, Focus::Concept(parent)),
            to: Focus::Concept(parent),
        })
        .collect();
    about.sections.push(section("in the hierarchy", up));
}

fn collection(l: &Librarian, id: Id<Collection>, about: &mut About) {
    let Some(c) = l.collections.get(&id) else {
        return;
    };
    let mut is = vec![fact(
        "shape",
        if c.ordered { "a sequence" } else { "a set" },
    )];
    if let Some(from) = &c.from {
        is.push(fact("frozen from", from.clone()));
    }
    about.sections.push(section("what it is", is));

    let held = crate::model::rows::Listing::entries_of(l, id);
    about.sections.push(section(
        "holds",
        vec![Row::Many {
            label: "recordings".into(),
            count: held.len(),
            query: format!("in={}", quoted(&c.name)),
        }],
    ));
}

fn work(l: &Librarian, id: Id<crate::model::entity::Work>, about: &mut About) {
    let of: Vec<Row> = l
        .performances_of(id)
        .into_iter()
        .take(AT_MOST)
        .map(|r| Row::Link {
            label: "performed by".into(),
            name: label_of(l, Focus::Recording(r)),
            to: Focus::Recording(r),
        })
        .collect();
    about.sections.push(section("recordings", of));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::instance::Instance;
    use crate::model::rows::Listing;

    /// One track, held, with a genre asserted twice and a third ruled out.
    fn library() -> (Librarian, Id<Recording>) {
        let mut held = crate::scan::Holdings::new();
        held.upsert(crate::fixture::Tagged {
            path: "/m/Dance/Luna/01 Callisto.flac".into(),
            title: Some("Callisto".into()),
            artist: Some("Stephan Bodzin".into()),
            albumartist: Some("Stephan Bodzin".into()),
            album: Some("Luna".into()),
            tracknumber: Some(1),
            ..Default::default()
        });
        let recording = {
            let l = crate::fixture::from_holdings(&held);
            let row = Listing::rows(&l)[0];
            Listing::recording_of(&l, row).expect("a fixture row is a recording")
        };

        let mut log = Instance::new(crate::fixture::device());
        let subject = Described::Recording(recording);
        log.assign_genre(
            100,
            subject,
            "Techno",
            Source::TagVariant,
            Confidence::Confirmed,
        );
        log.assign_genre(110, subject, "Techno", Source::Discogs, Confidence::Matched);
        log.assign_genre(
            120,
            subject,
            "Ambient",
            Source::Discogs,
            Confidence::Matched,
        );
        log.retract_concept(
            130,
            subject,
            crate::model::facet::GENRE,
            "Ambient",
            Source::Manual,
        );
        (
            crate::fixture::from_holdings_and(&held, log.records()),
            recording,
        )
    }

    fn rows<'a>(about: &'a About, section: &str) -> &'a [Row] {
        about
            .sections
            .iter()
            .find(|s| s.title == section)
            .map_or(&[][..], |s| s.rows.as_slice())
    }

    /// **Two sources saying one thing are two rows**, and each says who it
    /// was — the whole reason the catalogue keys assertions by source.
    #[test]
    fn a_claim_says_who_made_it_and_how_sure() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        assert_eq!(about.kind, "recording");
        assert_eq!(about.title, "Callisto");

        let said = rows(&about, "said about it");
        let techno: Vec<_> = said
            .iter()
            .filter(|r| matches!(r, Row::Claim { value, .. } if value == "Techno"))
            .collect();
        assert_eq!(
            techno.len(),
            2,
            "two sources collapsed into one row: {said:?}"
        );
        assert!(techno.iter().any(|r| matches!(
            r,
            Row::Claim {
                said: Some((Source::TagVariant, Confidence::Confirmed)),
                ..
            }
        )));
        assert!(techno.iter().any(|r| matches!(
            r,
            Row::Claim {
                said: Some((Source::Discogs, Confidence::Matched)),
                ..
            }
        )));
    }

    /// **A ruled-out claim is present and marked**, never missing (#96).
    ///
    /// "Somebody decided against this" and "nobody ever said it" are different
    /// answers, and an omitted row can only express the second.
    #[test]
    fn a_retracted_claim_is_shown_struck_rather_than_dropped() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        let ambient = rows(&about, "said about it")
            .iter()
            .find(|r| matches!(r, Row::Claim { value, .. } if value == "Ambient"))
            .expect("the retracted claim is missing entirely");
        assert!(
            matches!(
                ambient,
                Row::Claim {
                    retracted: Some(Source::Manual),
                    ..
                }
            ),
            "{ambient:?}"
        );
    }

    /// A claim points at the concept's own page, because a concept has one.
    #[test]
    fn a_claim_leads_to_the_concept_it_names() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        let to = rows(&about, "said about it")
            .iter()
            .find_map(|r| match r {
                Row::Claim { value, to, .. } if value == "Techno" => *to,
                _ => None,
            })
            .expect("a claim with nowhere to go");
        assert_eq!(
            to,
            Focus::Concept(Id::<Concept>::in_facet(
                crate::model::facet::GENRE,
                "Techno"
            ))
        );
        assert_eq!(l.about(to, true).title, "Techno");
    }

    /// The copies section reaches the bytes, and the bytes reach back.
    #[test]
    fn a_recording_and_its_copy_link_to_each_other() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        let copy = rows(&about, "copies")
            .iter()
            .find_map(|r| match r {
                Row::Link { to, .. } => Some(*to),
                _ => None,
            })
            .expect("no copy listed");
        assert!(matches!(copy, Focus::Encoding(_)));

        let back = l.about(copy, true);
        assert_eq!(back.kind, "encoding");
        assert_eq!(
            rows(&back, "of").iter().find_map(|r| match r {
                Row::Link { to, .. } => Some(*to),
                _ => None,
            }),
            Some(Focus::Recording(recording)),
            "the copy does not name what it is a copy of"
        );
    }

    /// **The facts say when, and on which machine** — #128.
    ///
    /// A claim's basis says who; this is the other half.
    #[test]
    fn the_facts_say_when_and_where() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        let facts = rows(&about, FACTS);
        assert!(
            !facts.is_empty(),
            "no history for a recording with four facts"
        );

        // Newest first: what changed recently is what somebody is looking for.
        let stamps: Vec<i64> = facts
            .iter()
            .filter_map(|r| match r {
                Row::Happened { at, .. } => Some(*at),
                _ => None,
            })
            .collect();
        let mut sorted = stamps.clone();
        sorted.sort_by(|a, b| b.cmp(a));
        assert_eq!(stamps, sorted, "the log is not newest first");

        assert!(
            facts
                .iter()
                .all(|r| matches!(r, Row::Happened { device, .. } if !device.is_empty())),
            "a fact with no machine behind it"
        );
    }

    /// **A retraction reads as a retraction**, not as one more thing somebody
    /// said. Otherwise the log says a genre was asserted twice and never that
    /// anybody took it back.
    #[test]
    fn a_withdrawal_is_marked_as_one() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        assert!(
            rows(&about, FACTS).iter().any(|r| matches!(
                r,
                Row::Happened {
                    withdrawal: true,
                    ..
                }
            )),
            "the retraction reads as an assertion: {:?}",
            rows(&about, FACTS)
        );
    }

    /// **A fact's name and fields come from its own wire form**, so a variant
    /// this code has never been told about still shows what it is and when —
    /// #106's rule for a stale reader, applied to a reader of the log.
    #[test]
    fn a_fact_shows_itself_without_being_taught_how() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        let named: Vec<&String> = rows(&about, FACTS)
            .iter()
            .filter_map(|r| match r {
                Row::Happened { name, .. } => Some(name),
                _ => None,
            })
            .collect();
        assert!(
            named.iter().any(|n| n.as_str() == "ConceptAsserted"),
            "{named:?}"
        );
        // And an id in a fact is resolved to what the library calls it, which
        // is a lookup rather than a per-variant match.
        assert!(
            rows(&about, FACTS).iter().any(|r| matches!(
                r,
                Row::Happened { detail, .. } if detail.contains("Techno")
            )),
            "an id was left as a uuid: {:?}",
            rows(&about, FACTS)
        );
    }

    /// **Asking for no history costs nothing**, and the section is still there
    /// to be opened.
    #[test]
    fn the_history_is_built_only_when_it_is_asked_for() {
        let (l, recording) = library();
        let without = l.about(Focus::Recording(recording), false);
        assert!(rows(&without, FACTS).is_empty(), "it was built anyway");
        assert!(
            without.sections.iter().any(|s| s.title == FACTS),
            "the heading went with it, leaving no way to open it"
        );
    }

    /// **An empty section is not drawn.** A panel of headings with nothing
    /// under them says the library knows less than it does.
    #[test]
    fn a_section_with_nothing_in_it_is_left_out() {
        let (l, recording) = library();
        let about = l.about(Focus::Recording(recording), true);
        assert!(
            about.sections.iter().all(|s| !s.rows.is_empty()),
            "{:?}",
            about.sections
        );
        // Nothing has been played, so history is still there — never is an
        // answer — but nothing performs a work, so that link is absent.
        assert!(rows(&about, "history").iter().any(
            |r| matches!(r, Row::Fact { label, value } if label == "played" && value == "never")
        ));
    }

    /// A focus the library has never heard of says so rather than drawing an
    /// empty page that looks like a thing with nothing known about it.
    #[test]
    fn a_focus_the_library_does_not_hold_says_so() {
        let (l, _) = library();
        let nowhere = Focus::Recording(Id::from_uuid(uuid::Uuid::from_u128(0xdead)));
        let about = l.about(nowhere, true);
        assert_eq!(about.title, "(not in the library)");
    }
}

#[cfg(test)]
mod perf {
    //! What it costs to open a panel — #125.
    //!
    //! `of` scans: the credits, the tracks, the entries. That is right for a
    //! question asked about **one** focus when a panel opens, and it is the
    //! thing #121 and #123 both got wrong by asking it per row. Measured so
    //! the claim is not merely asserted.

    use super::*;
    use crate::model::instance::{Instance, Scanned};
    use crate::model::rows::Listing;

    /// A library the size of a real one.
    fn library(artists: usize, albums: usize) -> Librarian {
        let mut scan = Vec::new();
        for a in 0..albums {
            for track in 1..=10u16 {
                let artist = format!("Artist {:04}", a % artists);
                scan.push(Scanned {
                    audio: crate::audio_id::AudioId {
                        scheme: 1,
                        hash: (a as u128) * 1000 + u128::from(track),
                    },
                    title: format!("Track {track:02} of {a:04}"),
                    artist: artist.clone(),
                    album: Some(format!("Album {a:04}")),
                    album_artist: Some(artist),
                    genres: vec!["Techno".into()],
                    disc: Some(1),
                    track: Some(track),
                    length_ms: 200_000,
                    ..Default::default()
                });
            }
        }
        let mut log = Instance::new(crate::fixture::device());
        log.ingest(100, &scan);
        let mut l = Librarian::from_catalogue(&log.catalogue());
        l.here(crate::fixture::device());
        l
    }

    /// **Opening a panel is a keystroke away from the screen**, so the number
    /// that matters is one focus, not a sweep.
    ///
    /// Generous bound, like `shelf::perf` and `seam::perf`: what it has to
    /// catch is a scan that grew a factor of ten, not a drift.
    #[test]
    fn opening_a_panel_costs_one_focus() {
        let l = library(200, 650);
        let rows = Listing::rows(&l);
        let recording = Listing::recording_of(&l, rows[0]).expect("a row is a recording");

        let worst = [Focus::Recording(recording), Focus::Encoding(rows[0])]
            .into_iter()
            .map(|focus| {
                let t = std::time::Instant::now();
                let about = l.about(focus, true);
                let took = t.elapsed();
                eprintln!(
                    "  {:<12} {took:>9.1?}  ({} sections)",
                    about.kind,
                    about.sections.len()
                );
                took
            })
            .max()
            .expect("two focuses");

        assert!(
            worst < std::time::Duration::from_millis(50),
            "opening a panel took {worst:?} — it is sweeping the library"
        );
    }
}

/// What naming a thing found — #129.
#[derive(Debug, Clone, PartialEq)]
pub enum Found {
    One(Focus),
    /// Nothing by that name.
    Nothing,
    /// **Several, and it does not choose.** Guessing which artist somebody
    /// meant is the same mistake as guessing whether a bare word is a genre or
    /// a crate (#119), and the repair is the same: hand them back.
    ///
    /// **Each with its focus**, so a caller can offer the choice rather than
    /// only report it — the names alone would make the picker go and find them
    /// again, which is a second search that could disagree with the first.
    Several(Vec<(Focus, String)>),
}

/// Resolve a name to a thing — #129.
///
/// **Exact first, then containment.** An id is minted from the exact credited
/// string, so `artist="Stephan Bodzin"` lands without a search; but nobody
/// types that, and `artist="Bodzin"` has to work. Case is ignored in both,
/// because a name is not a password.
pub fn find(l: &Librarian, of: Kind, name: &str) -> Found {
    let wanted = name.trim().to_lowercase();
    if wanted.is_empty() {
        return Found::Nothing;
    }
    let named: Vec<(Focus, &str)> = match of {
        Kind::Artist => l
            .reference
            .artists
            .iter()
            .map(|(id, a)| (Focus::Artist(*id), a.name.as_str()))
            .collect(),
        Kind::Collection => l
            .collections
            .iter()
            .map(|(id, c)| (Focus::Collection(*id), c.name.as_str()))
            .collect(),
        Kind::Concept(key) => l
            .vocabulary
            .genres
            .iter()
            .filter(|(id, term)| Id::<Concept>::in_facet(key, &term.name) == **id)
            .map(|(id, term)| (Focus::Concept(*id), term.name.as_str()))
            .collect(),
    };

    if let Some((focus, _)) = named.iter().find(|(_, n)| n.to_lowercase() == wanted) {
        return Found::One(*focus);
    }
    let near: Vec<(Focus, &str)> = named
        .into_iter()
        .filter(|(_, n)| n.to_lowercase().contains(&wanted))
        .collect();
    match near.as_slice() {
        [] => Found::Nothing,
        [(focus, _)] => Found::One(*focus),
        several => Found::Several(
            several
                .iter()
                .map(|(focus, n)| (*focus, (*n).to_string()))
                .collect(),
        ),
    }
}

/// What a name is a name *of*.
///
/// The three the `:` line can spell — `artist=`, `in=`, and any facet key.
/// A recording is deliberately absent: titles are not unique and the row under
/// the cursor is how you name one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Artist,
    Collection,
    Concept(&'static str),
}

impl Kind {
    /// What to call this when nothing answers to a name.
    ///
    /// **The kind is in the message**, because "nothing by that name" leaves
    /// the reader wondering whether they typed the wrong name or asked the
    /// wrong question — and the field they wrote already says which one this
    /// was looking for.
    pub fn word(self) -> &'static str {
        match self {
            Kind::Artist => "artist",
            Kind::Collection => "collection",
            Kind::Concept(key) => key,
        }
    }
}
