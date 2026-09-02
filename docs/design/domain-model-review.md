# Review of `domain-model.md` (combined draft)

**Status:** external critique for consideration, not a mandate. The draft is strong — the census-grounded claims, the
reversed-drafts honesty, and §7's comparative reading are unusually good. The points below are ordered by severity: one
substantive modeling error, one self-contradiction against the draft's own §2 rule, then gaps and drift. Each states its
evidence and its fix so it can be accepted, adapted, or rebutted on the merits. Section references are to the draft
under review.

---

## 1. Derivation is at the wrong level (substantive error)

The draft puts all derivation at `Work → Work` (§2 `Derivation`, §7 "the one place this model contradicts MO on
purpose", §9's cover/remix example). Two different mistakes are folded together.

### 1a. A cover is not a derivation at all

§9 models _Buckets of Rain_ as Dylan's Work with a **cover-derived second Work** realized by Neko Case. But three lines
up, the classical example does the opposite, correctly: one Work "Cello Suite No. 1", many Recordings (Rostropovich
1991, Casals 1936). Casals performing Bach and Neko Case performing Dylan are the same relation — a new Expression of
the same Work. The draft's own evidence agrees: Neko Case's file carries `composer: Bob Dylan` — the tag names the
_original_ work, not a derived one. FRBR, MusicBrainz practice, and the 2250-file composer census all point the same
way.

**Fix:** remove `Cover` from `DerivationKind`. A cover is a `Recording` realizing the existing `Work`. Remove `Live` for
the same reason — a live version is a Recording with a `recorded` date, not a derived composition.

### 1b. Signal-level derivations forced up to Work level

Remix, edit, dub, mashup, sample, and DJ mix are relations between _recordings_ (in MO's terms, signals — the draft's §7
even quotes `mo:remix_of: Signal → Signal` before overriding it). Lifting them to `Work → Work` creates three concrete
problems for this library:

1. **Work proliferation where Work coverage is thinnest.** Every "(Original Mix)" tell (191 versioned titles, 36
   multi-version bases) implies a phantom Work minted solely to hang a derivation on — in dance music, the genre where
   works are barely catalogued and the 16% unidentified skews.
2. **Unrepresentable cases.** A remix of a track whose Work is unknown cannot be recorded without inventing a
   provisional Work nobody will ever verify.
3. **The DJ instrument needs the version, and Work-level cannot name it.** A recorded mix's tracklist names _the Joris
   Voorn remix_, often a specific master. §10's "a mix is a Derivation of kind DjMix from many works" cannot answer
   "which version of Callisto is in this mix" — the question a DJ actually asks. Likewise sampling: Blawan sampled the
   Fugees' _recording_ of "How Many Mics", not the composition; a licensing or lineage query about samples is
   recording-level by nature.

**Fix (this is MusicBrainz's actual shape, which the draft claims):**

- `RecordingDerivation { from: Ref<Recording>, to: Ref<Recording>, kind }` for
  `{Remix, Edit, Dub, Mashup, Sample, DjMix}`.
- `WorkDerivation { from, to, kind }` retained only for genuinely compositional kinds:
  `{Arrangement, Medley, Quotation}`.
- A remix recording **realises the same Work as the original** (when known). "Every version of Callisto" is then
  recordings-of-work plus one hop of recording derivations — no transitive closure over phantom works.

**Bonus resolution:** this settles the remaster question the draft never poses. A remaster is the _same Recording_, a
new Release/Distribution — which is why `Remaster` is rightly absent from the kind enum. State that explicitly so nobody
adds it.

**Bearing on open question 9:** the pull toward splitting performance from signal ("a recorded DJ mix is literally a
performance") is fully satisfied by recording-level `DjMix` derivation. With that in place, decline the split:
MusicBrainz runs 30M recordings without it, and a fifth level that 84% of this library would leave empty buys nothing
here.

## 2. `ReleaseGroup.genres: Vec<GenreRef>` violates the draft's own §2 rule

It is a parent-side collection — exactly the colliding write-unit §2 spends four paragraphs prohibiting. Two devices
tagging different genres on one album both mutate the RG record; per-record LWW drops one silently.

It is also the wrong _subject_ and the wrong _shape_ for this library:

- **Wrong subject for two of the three collection types in scope.** On a VA or mix compilation (the dominant dance-music
  album form), each track's genre differs from the container's. In classical, an RG mixing works has no single genre.
  Genre is primarily a Recording fact; the RG value is a rollup.
- **Wrong shape for the workflow that already exists.** A bare `GenreRef` has nowhere to put per-assignment basis and
  confidence — which `genre-uncertain.tsv` already tracks and which §2's CatalogRecord discussion laments discarding at
  end of pass.

**Fix — one mechanism for all three problems:**

```
GenreAssignment {
    subject: SubjectRef,        // Recording primarily; ReleaseGroup as asserted rollup
    genre: GenreRef,
    basis: Source,              // tag, MusicBrainz, manual, inferred
    confidence: Confidence,
}
```

Membership as row existence → unions across devices; carries the review metadata natively; multi-genre falls out. The §5
hierarchy machinery is untouched — it operates on the vocabulary, not on where assignments live.

## 3. Artist–artist relations have no home

Declining `TypedEdge` for typed structs (§7) was right, but `Credit` and `Derivation` only cover artist→content.
Member-of and alias-of vanished with the generic edge and nothing replaced them. Dance music is _the_ genre of aliases;
the library's own "Alarm Will Sound performs Aphex Twin" names an artist with a dozen pseudonyms. "Everything by this
artist under any name" is a first-order query for this collection.

**Fix, same pattern as §1's:** `ArtistRelation { from, to, kind: MemberOf | AliasOf, period: Option<DateRange> }`.

## 4. ReleaseGroup and Release carry no artist credit

Credits attach only to Work and Recording, so "browse by album artist" — the most-used level in any player — must be
derived by aggregating recording credits, and the various-artists distinction that `ReleaseType` half-encodes has no
authoritative source. MusicBrainz puts an artist credit on both RG and Release for exactly this reason. The draft's own
war story — `label` ending up inside `albumartist` — happened because album-level credit had no proper home.

**Fix:** extend `Credit`'s subject to ReleaseGroup and Release (see §6 below for the subject type), or add a dedicated
album-credit; either way the album-artist level reads from an asserted fact, not an aggregation guess.

## 5. "Local, never travels" conflates write-ownership with visibility

If Distribution rows never leave their device (§2 entity table), then:

- the laptop cannot answer "is there a good copy **anywhere**" — `best_cutoff < 17k` as the re-buy list silently means
  "no good copy _here_";
- a play event's distribution reference (§4) dangles on every other device;
- open question 6 (canonical distribution) becomes unanswerable globally.

The sync design already has the correct shape for this: **device-scoped rows, each device solely authoritative for its
own, synced by union** like every other row-existence fact. Keep truly local mutable state (path, present, mtime)
unsynced; let facts about the bytes (audio_id, format, bitrate, cutoff, recording binding) travel as facts about device
X. This requires reinstating a small `Device` entity and a `device` field on Distribution.

## 6. Internal drift (small, cheap to fix now)

- §8's code sketch has `Recording.credits: Vec<Credit>` — the parent-side collection again, contradicting §2's rule and
  the class diagram (which correctly points Credit at its subject). The sketch is the drift; fix the sketch.
- `Credit` with `Option<Ref<Work>>` **and** `Option<Ref<Recording>>` represents both-none and both-some. Make the
  subject a sum type — `CreditOn::Work(_) | Recording(_) | ReleaseGroup(_) | Release(_)` (per §4 above) — so the illegal
  states are unrepresentable, which is this codebase's stated religion.
- §6 requires each Distribution to record its encoder delay for cue-point correction, but the field is missing from the
  entity's attribute list. Add `encoder_delay: Option<u32>`.

## 7. CatalogRecord is 1:1 with Recording, but provisionality isn't

The ingest ladder (§3) mints provisional **Artists** and **Works** from fuzzy matches; their confidence and verification
state need the same home as a recording's. DCAT's CatalogRecord — the draft's own source — is per catalogued resource of
any type. Generalize the subject, or provisionality becomes invisible exactly where it is most common.

## Positions on the open questions

- **Q1 (does Release earn its place): yes.** Catalogue numbers are the lingua franca of dance music; labels hang off
  Release; masterings differ by pressing; classical box sets need it. Keep both levels.
- **Q7 (Collection vs Playlist): unify.** A Collection is a Playlist with no meaningful `ord`. One entity, `ord`
  optional, one entry type instead of two.
- **Q8 (key vocabulary): the draft already answered itself** — one key codelist, Camelot as a second hierarchy over it.
  Promote from question to decision.
- **Q9 (performance/signal split): decline** — see §1b's bonus resolution; the DJ-mix pull is satisfied at recording
  level, and the remaster test shows the single class handles the hard case.

## The through-line

Points 1 and 2 share a structure: a correct **general** rule was adopted in prose (typed relations at the right level;
associations on the child) and then violated by one field or one worked example that predated the rule. The
census-driven audit method that caught `label`-inside-`albumartist` will catch these too. Two mechanical passes worth
running over the draft after each revision:

1. Every `Vec`/collection field in every struct and diagram, checked against §2's child-side rule.
2. Every worked example in §9, type-checked against the class diagram as if it were instance data — §9's cover example
   fails against §9's own classical example today.
