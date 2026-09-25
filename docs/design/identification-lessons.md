# Identification: what twenty years of retagging does, and the rules that survived it

**Status: a working document.** Not a design, not a decision log. It is the list of things that turned out to be true
about _this library's_ metadata, and the rules that came out of being wrong about them.

The problem this exists for: the input is bad, and it is bad in ways that defeat plausible rules. Files have been
tagged, retagged, ripped, re-ripped, bought, downloaded and DJ-pooled across two decades and several tools. **Every
field lies somewhere.** A rule that works on the first fifty files is not a rule.

Written while doing #160, which is the standing question of what the ingest flow should be.

---

## 1. The load-bearing lesson: a pass can confirm its own mistake

`joined` split `Chus & Ceballos, DJ Chus, Pablo Ceballos, Leonardo Gonnelli` into five names, two of which — `Chus` and
`Ceballos` — have never existed. The duo _is_ DJ Chus and Pablo Ceballos, so the credit is redundant and not wrong.

The pass has a corroboration step precisely so a bad split does not get acted on. **It agreed.** A sibling credit listed
the same wrongly-split parts in another order, `reordered` saw a match, and the verdict came out `list` — its strongest
verdict, the one that needs no other evidence.

> **Two derivations of one bad rule agreeing is not corroboration.** It is the rule agreeing with itself.

The existing test asserted the wrong answer, on the stated theory that a bad split is harmless because the verdict
decides. The theory was wrong in the one case that mattered.

**What to do instead:** corroborate against something the rule did not produce. A tag, an authority, a duration, a
person. Never against another output of the same pass.

---

## 2. Do not let one signal decide what another gets to see

`identify` ranked candidates by MusicBrainz's name score and kept three. **A remix cannot outrank the original it is a
remix of** — the titles differ only by a suffix, so the plain track always sorts first.

`How Many Times (Andhim Remix)`: the 3:39 original came back twice before any remix, and the four candidates matching
our 8:15 file _to the second_ sat at ranks 5, 7, 8 and 10. `best` selects on duration and never saw them. What it
reported was "name score 100 but duration off by 118s" — a veto against a candidate that was never the right one.

Measured on twelve rows the pass had left for a person: **five resolve at score 100 with a duration exact to the
second**, at ranks 4, 5, 6, 7 and 14.

> **When the discriminator is X, do not let a ranking on Y decide what X is allowed to consider.**

---

## 3. A blocklist of what junk looks like will miss junk

`[MARAPCANA.COM]` became an artist folder. The first fix was a scan for `.com`, `.net`, `.org`, `.ru`, `www.` and a
leading bracket. **It missed `minimalfreaks.pw`, which was in the library the whole time.**

And it cannot be tightened safely: `[untitled]` is a real Thomas Brinkmann album title, `Sunn O)))` is a band, `!!!` is
a band.

What worked is a question about _corroboration_ rather than appearance: **does anyone this album is credited to actually
appear on it?** Absent a MusicBrainz id vouching for the credit, an album credit sharing no member with any track on it
is a claim nothing supports. **11 of 733 album credits, and every one of them wrong** — two site stamps, a netlabel, a
bare `Various`, and two singles credited to an artist who is not on them.

> **Check what the evidence supports, not what it looks like.**

---

## 4. Compare members, never whole strings

The same mistake appeared three times in one evening.

- `joined` split on every joiner including `&` inside a comma list, folding a duo into two non-people.
- The album-credit check compared whole credit strings and called `Onno, Okain` and `Okain,Onno` a disagreement: **36
  findings, 25 of them that.** Split into members: 11, all real.
- `line_of` deduplicated credits by artist **id**, so one person holding two ids — one minted from a MusicBrainz id, one
  from the name — printed as `Anderson .Paak, Anderson .Paak` on 298 files.

> **A credit is a list. Comparing it as a string is comparing a spelling.**

And the corollary that keeps it honest: **the separator does not carry the answer.** `Drum & Bass` is one genre and
`Rock , Experimental` is two; `Itch-E & Scratch-E` is one act and `Armand Van Helden,Dizzee Rascal` is two. Of nine
genre terms carrying a separator, **eight are legitimate compound names covering 502 files.** A rule that split them
would be right once and catastrophic eight times.

---

## 5. A rule about a tag is a rule about one serialisation

#145 guards against a joined credit renaming the artist it merely begins with. The guard asks whether the artist-id
field contains a `/` — which is **a Picard convention for packing several ids into one ID3 frame**.

A Vorbis comment repeats the field instead. `Trio` carries three separate `musicbrainz_albumartistid` entries, no slash
anywhere, so the guard read `false` and let the credit rename Dolly Parton globally: **six of her albums filed under a
name only one of them has.** Measured: 72 mp3 files slash-joined where the guard worked, 11 ogg files repeated where it
did not.

> **Ask what the tag means, not how one tagger spelled it.**

---

## 6. Last-writer-wins means a repair cannot be a repeat

`ArtistNamed` is last-writer-wins over one key. So the _right_ name already being in the log is exactly what stops it
winning: `apply`'s replay guard drops it as a duplicate, and `shelf import` on the affected folders reports
`0 new fact(s)` and changes nothing.

**A re-import cannot repair a name.** Only a new assertion outranks the wrong one, and the guard had to learn the rule
the paired facts already knew: _"already in the log" is not "still true"_.

> **For any fact where the last word wins, judge a proposal against the current state and never against the history.**

---

## 7. Derived files rot, and rotted files are indistinguishable from work

`joined-propose.jsonl` held 339 proposals. **258 were artefacts of a bug fixed hours earlier** — the pass matches
against the library's credit line, which said `Bob Dylan & The Band` on 165 Dylan recordings. Regenerating over the same
library gave 48.

An audit of `tools/out` found **19 of 30 files were records of finished work**, including 5,774 rows of genre snapshot
and 429 identifications against a folder tree that no longer exists. The mechanism was one line: a survey that found
nothing did not overwrite the last one that found something, so a clean library went on being described by a stale file.

> **Regenerate before trusting. A file cannot tell you it is stale.**

---

## 8. The evidence is destroyed by the pipeline that reads it

147 files carry a `_pn` suffix recording that a mastering tool re-encoded them. **The library holds none of them**, and
would hold none even after import: `shelf render` names a placed file from the library, so the marker is gone the moment
it is placed. The cutoff test cannot recover it — a competent 320-to-320 pass leaves no wall, and 130 of the 144 marked
files measure clean.

Same shape: the ID3v1 truncation detector needs the 128-byte block to still be there, and `tags apply` strips it.

> **Read what only the path knows while the path still exists, or accept that it never entered the library.**

---

## 9. The tags lie about which release a track is from

`Till Dawn` by Roberto Capuano is on the `Wilford EP`. Its tags say the album is `Beatport Techno Top 100 December 2015`
— **a chart it appeared on, not a release**. Its album artist was a warez site.

Every external route in the flow is album-shaped, and all of them fail on that: `ingest` needs an exact tracklist match
(a 1-track slice of a 100-track chart can never match), `genre fetch` follows a MusicBrainz release relationship the
file does not carry, and its fallback searches Discogs by album name.

**One MusicBrainz _recording_ search on `artist` + `title` returns it at score 100, on `Wilford EP`.** The evidence was
in the file the whole time; nothing looked at it that way.

> **On DJ-pool and chart-ripped material, `artist` and `title` are the reliable fields and the album fields are the
> least reliable. The lookups are shaped the other way round.**

---

## 10. Applying an identification twice is not applying it twice

A relink from a **byte-minted** recording merges: the old id was "nothing had named this", so the two are one
performance badly named and everything hanging off the old one comes across. A relink from a recording an authority had
**already named** does not merge, and must not — two MusicBrainz recordings are two different performances.

Which means a second identification pass, finding a *better_ answer than the first, relinks
`byte → MusicBrainz-A → MusicBrainz-B` — and **the description stays on A**. `Identified` carries no description by
design: "a title from MusicBrainz is a separate claim somebody else can propose", and nothing proposes one.

Measured: applying a window-of-3 pass and then a window-of-25 pass left **24 files bound to a recording with no title,
no credit and no computable destination**. They were invisible in the counts except as `held elsewhere` going from 6
to 30.

The repair was to roll back and apply the better pass **once**, from the original state, so every relink goes
byte-minted → named and the merge carries the description.

> **Identify from the original state, once. A second pass over already-identified rows strands what the first one
> carried.**

And the near-miss worth naming: this was found by chasing a count that moved by 24 for no stated reason. Nothing failed,
nothing was reported, and 24 rows had quietly lost their names.

---

## 11. Rolling the log back does not roll the files back

`shelf render --write` had renamed `10 Wir.mp3` to `10 Wir (Original Mix).mp3` from an identification. Restoring the log
to before that identification left the library believing the old name while the disk held the new one — one file
`blocked`, wanting to move to a path it was already occupying.

`shelf import` with no arguments is the repair: it walks the roots and republishes the holdings, which is the one fact
that says where a copy _is_.

> **A log backup restores the claims, not the bytes. After a rollback, reconcile.**

---

## 12. Two remixes of one track are the same length

`Slip Away (Larse Vox Remix)` is 518 seconds. Our Olivier Giacomotto remix is 517. A rule that takes the candidate
closest in duration files one as the other, confidently, at score 84.

Duration is the discriminator that separates a recording from its radio edit, and it separates nothing at all among
remixes of one track — they are the same piece of music at the same tempo, and there is no reason for them to differ.

> **What corroborates a remix is a candidate whose own title names the remixer.** Where several fit the length and none
> claims it, the length has decided nothing and the answer is a person.

Applied here that pushed `uncertain` from 29 to 46 — **refusals on purpose**, not a regression. 57 matches came back
confirmed by the remixer being named: `Elderbrook — How Many Times (Andhim remix)` at 495s against our 495s.

---

## 13. The two catalogues disagree about who a remix is by

MusicBrainz credits the recording to the **original** artist and holds the remixer as a relationship. A DJ tag puts the
**remixer** in `artist`, frequently as the only name there.

So `Andhim — How Many Times (Andhim Remix)` asked MusicBrainz about Andhim, who did not record it: 8 candidates, none
naming a remix. Asked about Elderbrook: 16, including the one that names Andhim's.

**And the suffix has to leave the query too.** `recording:"Slip Away (Olivier Giacomotto Remix)"` matches nothing at all
— MusicBrainz does not store it the way Beatport writes it — so keeping it turns a findable record into "no match",
which is indistinguishable from the record not existing.

Of 510 unidentified rows, 406 carried a bracketed version or remix. Fixing the question took `nothing` from **196 to
156** and `matched` from 258 to 283.

> **Ask the authority the question in the terms the authority uses.** A query that cannot match is not evidence of
> absence.

---

## 14. A refusal that is not printed is a line that vanished

`shelf apply` counted refusals nowhere and printed them never. 57 lines the vocabulary refused disappeared from a file
of 340 and the summary read `283 proposals` — which is exactly what a file with 283 lines in it looks like. Finding out
what happened meant splitting the file by kind and applying each half by hand.

The refusal itself was correct and useful: `Proposal::Credited` is the _split_ — "this credit names several artists" —
so it declines a single name, and it asserts `performer`, which a remixer must not be. One line of output would have
said so.

> **Every line a pass declines to act on has to be visible in what it prints.** A count that omits a category reads as a
> smaller file, not as a category.

---

## 15. What the numbers actually are

Kept because every rule above was written against a guess first and a measurement second.

| what                                                       |                                                        |
| ---------------------------------------------------------- | ------------------------------------------------------ |
| recordings resting on their bytes, before / after identify | **522 (7.5%)** to **250 (3.6%)**                       |
| album credits naming nobody who is on the album            | 11 of 733                                              |
| artist ids that have been named more than one thing        | 19 of 511                                              |
| albums held as more than one release group                 | 50, of which 48 were dead identities                   |
| credits in one DJ crate that look joined                   | 169 of 434, covering 212 files                         |
| genre terms carrying a separator that are one real term    | **8 of 9**                                             |
| files whose filename records a re-encode the tags do not   | 147, **0 of them in the library**                      |
| distinct genre terms before / after one case fix           | 242 / 240, with 70 files respelled                     |
| unidentified rows carrying a bracketed version or remix    | **406 of 510** — the shape the query finds nothing for |

---

## 16. The rules, in one place

1. Corroborate against something the rule did not produce.
2. When the discriminator is X, do not let a ranking on Y decide what X sees.
3. Check what the evidence supports, not what it looks like.
4. A credit is a list; compare members.
5. The separator does not carry the answer.
6. A rule about a tag is a rule about one serialisation.
7. For last-writer-wins facts, judge against the state, never the history.
8. Regenerate derived files before trusting them.
9. Read what only the path knows before the path is gone.
10. Identify from the original state, once.
11. A log backup restores the claims, not the bytes.
12. Duration separates a recording from its edit and separates nothing among remixes of one track.
13. Ask the authority the question in the terms the authority uses; a query that cannot match is not absence.
14. Every line a pass declines to act on has to be visible in what it prints.
15. **Prefer refusing to guessing** — a fold is the one thing this model cannot repair, and `library-api.md` §7 already
    says a library that accepts anything and marks it provisional fills with entries nobody will ever fix.
16. A percentage over one item is not agreement — a gate's strength is a property of its input, not of the rule.

---

## 17. How the mistakes were actually caught

Worth recording, because it is not flattering to the tests.

Of the errors above, **the tests caught one** (the untagged branch dropping `_pn`). The rest were caught by reading what
a pass would do before applying it, by looking at the library after applying it, or by a question about a specific
record — `why is [MARAPCANA.COM] an artist`, `did you look up the andhim remix`. Two passes reported their own wrong
answers confidently.

> **A review step that a person actually reads is load-bearing, not ceremony.** The value of a dry run is not that it is
> safe; it is that it is legible.

---

## 18. The regex that shapes a question is not the bar for asserting an answer

Finding the original a remix was made out of turned out to need no new source. `asked_about` already strips the remix
suffix and swaps in the original artist, so the search **is** the query for the original — it was in the result set the
whole time and was being thrown away. It is picked out by what its title does not say: no version qualifier at all, or
an explicit `(original mix)`, and exactly one such candidate.

Measured over the 56 remixes identified so far: **23 leave exactly one unqualified candidate**, 9 leave none, and 24
leave several at different lengths — four `Slip Away` recordings between 331s and 388s with nothing to say which one
Giacomotto remixed. Duration cannot help, because an original and its remix have no expected relationship in length. So
ambiguity refuses, and over the whole library the rule produced **24 derivations, the first this model has ever held**.

The mistake was in the other half of the same pass. `REMIXED_BY` matches a bracket ending in `mix` or `edit` as well as
`remix`, which is right for _stripping_ a suffix — a query is no worse for losing a segment that turned out to be a
remaster note. It is wrong for _crediting_, and the same regex was doing both. Run over the whole library it proposed
nine artists nobody is called: `reggae`, `single` twice, `alternate`, `full`, `Pro`, `Vox`, `monotone` — every one of
them from a bare `(X mix)`.

`qualifier::REMIXER_WORDS` had already excluded `mix` and `edit` on measured grounds — of the segments ending in `mix`,
**71 were version and remaster notes** against 65 ending in `remix` that were real people. The Rust side knew; the tool
had drifted.

> **Widening a query is free and widening an assertion is not.** The same string can be good enough to search with and
> nowhere near good enough to claim with, so the two bars belong in two places even when they read from one field.

Requiring a real remixer word defers 11 correct credits — `Dub Pistols mix`, `The Stingray mix`, `1969 Glyn Johns mix` —
to refuse those nine. That is the trade this library prefers, and the 11 are waiting for evidence better than a bracket
rather than lost. Three names the fix does not reach (`Mindless`, and two the run no longer emits) are a genre and two
production treatments, and a blocklist of those would be exactly the sneaky rule §15 says should fail and wait.

---

## 19. A fingerprint is not self-corroborating

The rung #57 was always described as the strong one: a fingerprint does not care how the tag is spelled, so it finds
what a name search cannot. That is true, and it is not the whole shape of the thing.

**AcoustID's score is not a measure of the answer.** It says how closely this audio matches a fingerprint AcoustID has
stored. The MusicBrainz id attached to that fingerprint was submitted by whoever ran a tagger, and a wrong submission is
permanent. So a score of 1.00 means "this is certainly that fingerprint" and says nothing about whether the fingerprint
is labelled correctly.

Built on score and duration alone, the first pass over 203 files returned **51 confident matches, of which roughly
fifteen were wrong** — and wrong in ways no reasonable person would accept:

| ours                                 | what the link said                             | score |
| ------------------------------------ | ---------------------------------------------- | ----- |
| Mars Bill — Motorcity                | Profetus — Dead Are Our Leaves of Autumn       | 1.00  |
| Hey Sunshine (Antonio Giacca Remix)  | Electric Light Orchestra — Roll Over Beethoven | 1.00  |
| Darius Syrossian — U+I               | Jivemaster — Sonic 3D Blast OC ReMix           | 1.00  |
| Slip Away (Olivier Giacomotto Remix) | Goldfish — Moonwalk Away (Lexer remix)         | 0.99  |

The design error was mine and it was reasoned, not careless. I turned the title check **off** on purpose, on the
argument that the audio had already matched — so a differing title must be our tag being wrong, which is exactly the
case the fingerprint exists to catch. `Trixi B2` really is MusicBrainz's `Trixi 1`. The argument is right about the
cases it was built from and wrong about the population.

> **The names are circular evidence on a name search and independent evidence on a fingerprint.** On the name path the
> search was _by_ name, so agreement says the search worked rather than that the record is right, and duration is the
> only independent axis. On the fingerprint path the search was by audio, so the names are a second axis and the
> **only** check available on a crowd-sourced link.

So the check comes back, loosely: the artist must **share one name** — `Andhim, Super Flu` against `Andhim` passes — and
the title must contain the other or read close to it. `Mr. Bass feat. Super Flu (Original Mix)` against `Mr. Bass`
passes; `Motorcity` against `Dead Are Our Leaves of Autumn` does not. **32 of the 51 survive, and the 19 refused include
every wrong one.**

**`sources` was measured and is not enough on its own.** A link submitted once is weaker than one submitted fifty times,
and `sources: 1` does flag three of the bad matches — but eight more had two or more sources and were still wrong. It is
a signal, not a gate.

**And the accent fold is load-bearing.** `Lvis Mejía` reaches this library as `Lvis Meji` + U+00B4 + `a`, and a
normaliser that deletes non-ASCII maps those to `lvismejia` and `lvismeja`, which do not match. The first version of the
name check refused both of that artist's tracks for that reason alone — a correct match thrown away by the guard meant
to protect it. Fold to ASCII through NFKD; do not strip.

---

## 20. A gate's strength is a property of its input

`ingest` accepts only an exact tracklist agreement, and that is a strong check because a tracklist is hard to match by
accident. On a **one-track release it is one title compared to one title**, and it prints the same word it prints for
thirteen:

```text
tracklist: exact (100%), 1 ours vs 1 theirs
```

Measured while bringing in four dangling singles on 2026-09-25. MusicBrainz returns nine releases for `Burn` / Ellie
Goulding, of which **two are one-track and titled exactly `Burn`**; both score 100 and both satisfy `exact` identically.
The pass took the first, which is **Withdrawn**. The other is **Official**, and `status` was in the same response that
named the winner.

**Identity survived it, and not by design.** Both pressings carry recording `87257b49`, and a recording is what a row
hangs on, so the arbitrary choice was invisible. `Come With Me Now` landed the same way on a **Promotion** pressing. The
verdict that reads strongest anywhere in the flow is, on this material, a coin flip that happened to land well twice.

> **`exact` describes the comparison, not the evidence.** A percentage over one item carries no information, and a gate
> cannot report confidence without also reporting how much there was to agree about.

**This sharpens #163 rather than repeating it.** #163 observes that a one-track slice of a 100-track chart can never
match a release, and concludes that an album-shaped miss now costs _effort_ rather than correctness, because the refusal
is safe. The one-track _release_ is the other half of that shape and it does not refuse — it agrees, at full confidence,
on almost nothing. A miss costs effort; a gate that cannot fail costs correctness. What kept that cost off the log here
was #160's recording-id requirement downstream, not anything the gate did.

**The independent axis was free and is unread.** The file's `TSRC` is `GBUM71303482`; MusicBrainz serves `isrcs` on a
recording and holds exactly that for `87257b49`, against `GBUV71300663` for the rival 238s `Burn`. So the ISRC
corroborates the winner and rejects the loser on an axis that is neither the name the search was made with nor the
duration — the §2 and §19 property that makes an axis worth having. Nothing reads it: `isrc` does not appear in the Rust
side at all, and `external` reads only Beatport and YouTube ids out of URL and comment frames, which is #100's
shape-blindness turning up in a second field.

**Measured before proposing it**, because the obvious use is the wrong one: of 600 sampled library files, **428 (71%)
carry an ISRC** and 587 (98%) carry a MusicBrainz recording id, with only **5** holding an ISRC and no recording id. As
a rung on #89's ladder it would earn five files. As corroboration for a thin match it can speak for 71% of the library.
Those are different jobs, and the coverage number that matters is different for each.

---

## 21. Related

#160 (what the ingest flow should be — the standing question this feeds) · #57 (AcoustID, the rung the ladder is
missing) · #143 (a path is evidence, on the weakest rung) · #152, #157 (joined credits, in the artist field and the
genre field) · #20, #51 (singles, which have no album-shaped stage to be _checked_ by — §20) · #163 (the album-shaped
passes) · #100 (an id in a frame shape nothing reads) · `library-api.md` §7 (the strictness that was written and not
implemented)
