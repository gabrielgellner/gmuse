#!/usr/bin/env python3
"""Find the MusicBrainz recording id for recordings that rest on their bytes.

## The contract, and why this pass no longer walks a disk

```sh
gmuse library query --unidentified > work.jsonl   # gmuse says what needs it
gmuse-tools identify fetch   work.jsonl           # ask MusicBrainz
gmuse-tools identify propose work.jsonl           # write proposals
gmuse library apply out/identify-work.jsonl       # dry run
gmuse library apply out/identify-work.jsonl --write
```

**A tool is handed an id and a locator, opens the locator, and proposes
against the id.** This pass used to walk the library itself and emit
`{"locator": "/Users/.../01 The Fugitive.mp3"}`, which is why 712 confident
matches sat in `tools/out/` that `gmuse library apply` could not read: a path
is a property of one machine's copy, stale the moment anything moves, and it
names nothing the library knows about.

Reading the work list also means *which* recordings need looking up is the
library's answer rather than this pass's guess, so the two cannot disagree —
and a recording an authority has already named never costs a rate limit.

**The highest-leverage pass there is, and the reason is a census.** Of 6500
files in `~/MusicLibrary`, 6023 carry a MusicBrainz recording id and 477 do
not — and those 477 are Adele, Stromae, James Blake, Stephan Bodzin and
Burial. Not obscure music. MusicBrainz has them; nobody has ever asked. The DJ
crate is the same story from a worse start: **zero** of 592 files carry an id,
and a sample says MusicBrainz has about 62% of them.

So the gap is a *tagging* gap, not an *identity* gap, and the two have
different remedies. A tagging gap closes with a lookup. An identity gap does
not close at all — see #89 for what happens to the residue, which is roughly
3% of everything and is mostly remixes, edits and dubs by small producers.

## Why the query needs normalising, with evidence

A first attempt reported that MusicBrainz had 3 of 30 crate tracks, which was
wrong, and wrong in two ways that matter:

- **`artist:` does not match a joined credit.** `"Stephan Bodzin & Marc
  Romboy"` scores zero; `"Stephan Bodzin"` scores 100. Beatport writes the
  full credit into `TPE1` and MusicBrainz indexes each artist separately.
- **`(Original Mix)` is not a MusicBrainz convention.** It is Beatport's word
  for "not a remix", and MusicBrainz simply calls that the recording.

Normalising both takes the crate from 3/30 to 19/30. The suffix strip is
deliberately narrow: `(Nolah Remix)` names a *different recording* and must
survive, so only "original mix" and "original" are removed.

## Duration is the second opinion

A title and artist match is a name match, and a name match can be wrong — the
same title by the same artist is a live take, a re-recording, or a radio edit.
Every candidate is therefore checked against the file's own duration, which
costs nothing and is the difference between `matched` and `probable`.

## Nothing is written

This fetches and proposes. It writes no tags and touches no audio. The raw
MusicBrainz responses are cached verbatim (#58) so the slow part is paid once:
at one request per second, the two roots together are about twenty minutes.
"""

from __future__ import annotations

import collections
import json
import re
import subprocess
import unicodedata
from dataclasses import dataclass
from difflib import SequenceMatcher
from pathlib import Path
from typing import TYPE_CHECKING, Any, Final

import mutagen
import typer

from gmuse_tools import bronze
from gmuse_tools.genre.common import out_path, secret

if TYPE_CHECKING:
    import sqlite3

USER_AGENT: Final = {"User-Agent": "gmuse-identify/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}
MB_RECORDING: Final = "https://musicbrainz.org/ws/2/recording"
HTTP_OK: Final = 200
#: How many candidates to ask for.
#:
#: **It was three, and three loses every remix.** MusicBrainz ranks by name, and
#: a remix cannot outrank the original it is a remix *of* — the titles differ
#: only by a suffix, so the plain track always sorts first. `How Many Times
#: (Andhim Remix)` returned the 3:39 original twice before any remix, and the
#: four candidates matching our 8:15 file to the second sat at ranks 5, 7, 8
#: and 10. The window threw them away before `best` ever saw them.
#:
#: **Widening is safe because duration selects, not the ranking.** `best` takes
#: the candidate closest in length among those scoring `PLAUSIBLE` or better,
#: within `TOLERANCE` seconds. More candidates give that choice more to work
#: with; they do not loosen it. The note this replaces called raising the limit
#: "a different question with a real cost" — measured, on twelve rows this pass
#: left for a person, **five resolve at score 100 with a duration exact to the
#: second**, at ranks 4, 5, 6, 7 and 14.
#:
#: It costs no extra request either: the limit is a parameter on the one search
#: already being made.
CANDIDATES: Final = 25
#: A Lucene score at or above this is a name match worth considering. Below it
#: MusicBrainz is guessing from a partial term.
STRONG: Final = 95
#: A name that is plausible but not top-ranked. **On its own this decides
#: nothing** — it exists so a candidate whose *length* matches to the second is
#: not thrown away for scoring 80 on its title, which is what happened to
#: `Triton` (ours 463.9s, MB's 465.0s at score 80, discarded in favour of a
#: 222-second edit scoring 100).
PLAUSIBLE: Final = 70
#: Seconds of disagreement still counted as **the same recording**. Encoder
#: padding and a trimmed lead-in account for one or two; five is generous and
#: still rules out the live take.
# Which verdicts write an applying line rather than a commented-out one.
#
# `probable` is in because it means the name is strong and nothing contradicts
# it — the duration is simply unknown, which is a gap in the evidence rather
# than evidence against. `uncertain` is out by definition.
APPLIES: Final = frozenset({"matched", "probable"})

SAME_RECORDING: Final = 5.0

#: Seconds within which two titles differing only by a version qualifier are
#: **the same mix**.
#:
#: **Tighter than `SAME_RECORDING`, and it must stay tighter.** Muting a channel
#: does not change a length: an instrumental and its vocal are the same
#: duration *by construction*, which is exactly why duration cannot separate
#: them and why the title qualifier has to. Widen this and a radio edit starts
#: reading as an instrumental.
SAME_MIX: Final = 2.0

#: How far apart two recordings may be and still be plausibly **versions of one
#: song**.
#:
#: A ratio rather than a window, because the question is a different one. A
#: radio edit and an extended mix are the same song at 3:30 and 7:00, and no
#: number of seconds expresses that; what it excludes is a DJ mix that happens
#: to contain the track. The two constants above ask *is this the same thing*;
#: this one asks *is this a relative of it*, and unifying them would be
#: answering one question with the other's answer.
SIBLING_SPAN: Final = 3.0

#: What separates the artists inside one credit string.
CREDIT_SPLIT: Final = re.compile(r"\s*(?:,|&|;|\bfeat\.?\b|\bft\.?\b|\bvs\.?\b|\bx\b|/)\s*", re.IGNORECASE)
#: Beatport's way of saying "not a remix". Narrow on purpose: a real remix name
#: identifies a different recording and has to survive.
NOT_A_REMIX: Final = re.compile(r"\s*[([](?:original(?:\s+mix)?)[)\]]\s*$", re.IGNORECASE)
#: Who a bracketed segment says remixed this. `(Olivier Giacomotto Remix)`,
#: `(Andhim remix)`, `(Bedrock Vocal Mix)` — the name is what precedes the
#: word, and the word itself is never part of it.
REMIXED_BY: Final = re.compile(r"[([](?P<who>[^)\]]+?)\s+(?:re-?mix|rework|edit|mix)[)\]]", re.IGNORECASE)
#: Words that end a remix credit without being part of the name. Mirrors
#: `qualifier::NOT_A_NAME` on the Rust side.
NOT_A_NAME: Final = frozenset(
    {
        "radio",
        "club",
        "extended",
        "vocal",
        "dub",
        "instrumental",
        "original",
        "album",
        "single",
        "main",
        "short",
        "long",
        "remix",
        "mix",
        "edit",
        "vox",
        "alternate",
        "full",
    }
)

#: The bracket endings that credit **a person**, mirroring `qualifier`'s
#: `REMIXER_WORDS` — and `mix` and `edit` are absent from it on purpose.
#:
#: Rust measured this and this pass did not, which is the whole of why they
#: disagreed: of the bracketed segments ending in `mix`, **71 were remaster and
#: version notes** — `1969 Glyn Johns`, `2021`, `Boy Wunder radio` — against 65
#: ending in `remix` that were real people. `REMIXED_BY` stays wide because
#: *stripping* a suffix from a query is harmless whatever the suffix meant;
#: **asserting** who made something is not, and a title is the only evidence
#: there is.
#:
#: Measured here over the whole library: requiring this defers 11 real credits
#: and refuses 9 artists nobody is called — `reggae`, `single`, `alternate`,
#: `full`, `Pro`, `Vox`, `monotone` — plus the one derivation built on them.
#: The 11 are not lost, they are waiting for evidence better than a bracket.
CREDITS_A_PERSON: Final = re.compile(r"\b(?:re-?mix|rework|bootleg|refix|vip)\s*[)\]]", re.IGNORECASE)


def first_artist(credit: str) -> str:
    """The first artist in a credit string.

    MusicBrainz indexes artists individually, so a joined credit matches
    nothing. This is lossy and deliberately so: the remaining names are still
    checked against the candidate's own credit in `score_candidate`.
    """
    parts = [p for p in CREDIT_SPLIT.split(credit) if p.strip()]
    return parts[0].strip() if parts else credit.strip()


def searchable_title(title: str) -> str:
    """The title with Beatport's "not a remix" suffix removed."""
    return NOT_A_REMIX.sub("", title).strip()


def asked_about(tags: dict[str, str], stem: str) -> tuple[str, str, str | None]:
    """The artist, title and remixer this file should be searched by — #161.

    **The remix swap.** A remix is a different recording credited to somebody
    else, and the two catalogues disagree about who: MusicBrainz credits the
    *original* artist and holds the remixer as a relationship, while a DJ tag
    puts the remixer in `artist` — often as the only name there.

    Measured: `Andhim — How Many Times (Andhim Remix)` asked about Andhim
    returns 8 candidates; asked about Elderbrook it returns 16, including
    `How Many Times (Andhim remix)` at 495s against our 495s — the one
    candidate that *names the remixer*, which is the corroboration duration
    cannot give.

    **The remix segment leaves the query too.** `recording:"Slip Away (Olivier
    Giacomotto Remix)"` matches nothing at all: MusicBrainz does not store the
    suffix the way Beatport writes it, so keeping it turns a findable record
    into "no match". It comes back as the *check* rather than the question.
    """
    raw = tags.get("title", "") or stem
    remixer = remixer_in(raw)
    title = searchable_title(REMIXED_BY.sub("", raw).strip() if remixer else raw)
    return first_artist(without(tags.get("artist", ""), remixer)), title, remixer


def remixer_in(title: str) -> str | None:
    """Who a title says remixed this, if it says so — #161.

    **A remix is a different recording credited to somebody else**, and the
    two catalogues disagree about who. MusicBrainz credits the recording to
    the *original* artist and holds the remixer as a relationship; a DJ tag
    puts the remixer in `artist`, often as the only name there.

    So `Andhim — How Many Times (Andhim Remix)` asks MusicBrainz about Andhim,
    who did not record it. Asked about Elderbrook instead, the answer comes
    back naming the remixer in its own title — which is the corroboration
    duration cannot give.
    """
    m = REMIXED_BY.search(title)
    if not m:
        return None
    who = m.group("who").strip()
    # Trailing version words are not part of a name — the same rule
    # `qualifier::credits_in` follows on the Rust side, and for the same
    # reason: `Hi-Bias Signature radio remix` credits nobody called `radio`.
    while who and who.rsplit(" ", 1)[-1].lower() in NOT_A_NAME:
        cut = who.rsplit(" ", 1)
        if len(cut) == 1:
            return None
        who = cut[0].strip()
    return who or None


def without(credit: str, name: str | None) -> str:
    """The credit with one name taken out, where anything is left.

    **The swap #161 is about.** For a remix the tag's artist is frequently the
    remixer and MusicBrainz credits the original, so asking about the remixer
    asks the wrong question. Where the remixer is the *only* name, it is left
    alone: a query naming nobody returns 25 unrelated candidates and no
    duration match at all, which is worse than asking imprecisely.
    """
    if not name:
        return credit
    wanted = plain(name)
    kept = [p.strip() for p in CREDIT_SPLIT.split(credit) if p.strip() and plain(p.strip()) != wanted]
    return ", ".join(kept) if kept else credit


def flatten(value: object) -> str:
    """One string, whatever the container handed back.

    **Vorbis comments are multi-valued**, so mutagen returns a list — and
    `str()` on a list is its *repr*. Five Opus files were queried as
    `recording:"[The Mountain]"`, which matched nothing and read as
    MusicBrainz not having them. ID3 frames stringify to their text, so this
    only bites the containers where a field legitimately has several values.
    """
    if isinstance(value, list):
        return "; ".join(str(v) for v in value)
    return str(value)


def read(path: Path) -> tuple[dict[str, str], float | None]:
    """A file's tags and its playing time.

    **Not `genre.common.tags_of`, and the reason is a defect this found.**
    `tags_of` shells out to ffprobe, and **ffprobe does not expose the `UFID`
    frame** — which is where ID3 keeps the MusicBrainz *recording* id. Through
    ffprobe a fully tagged file looks unidentified: it shows
    `musicbrainz release track id` and nothing else, and that is a different
    id entirely, unique to one release.

    Left unnoticed, this pass would have asked MusicBrainz about all 6038 files
    rather than the 477 that need it — two hours of somebody else's rate limit
    spent relearning what the files already said.

    So this reads with mutagen, which sees `UFID`. It is also far faster, since
    it does not spawn a process per file.
    """
    try:
        handle = mutagen.File(path)
    except Exception:  # noqa: BLE001 -- a corrupt file is data here, not an error
        return {}, None
    if handle is None:
        return {}, None
    raw = {str(key).lower(): flatten(value) for key, value in dict(handle.tags or {}).items()}
    length = getattr(handle.info, "length", None)
    return normalise(raw), (float(length) if isinstance(length, (int, float)) else None)


#: What each container calls the same field. **The reason this exists is a
#: defect.** Switching from ffprobe to mutagen traded one problem for another:
#: ffprobe could not see `UFID`, and mutagen reports ID3 by its raw frame ids,
#: so `tags["artist"]` came back empty for every MP3 in the library. The lookup
#: then queried MusicBrainz with no artist and a filename for a title, and
#: reported that MusicBrainz had none of 591 dance tracks — a result that was
#: wrong in the same direction as the earlier query bug, and for the same
#: reason: a normalisation gap reads as a gap in the source.
NAMES: Final = {
    "title": ("tit2", "\xa9nam", "----:com.apple.itunes:title"),
    "artist": ("tpe1", "\xa9art", "----:com.apple.itunes:artist"),
    "album": ("talb", "\xa9alb"),
    "albumartist": ("tpe2", "aart"),
}


def normalise(raw: dict[str, str]) -> dict[str, str]:
    """One name per field, whatever the container calls it.

    Vorbis and Opus already use the plain names, so they pass through; ID3 and
    MP4 are mapped. Raw keys are kept alongside, because `has_recording_id`
    needs `ufid:...` exactly as it is written.
    """
    tags = dict(raw)
    for name, spellings in NAMES.items():
        if tags.get(name):
            continue
        for key in spellings:
            if raw.get(key):
                tags[name] = raw[key]
                break
    return tags


def has_recording_id(tags: dict[str, str]) -> bool:
    """Whether the file already names its MusicBrainz **recording**.

    Three spellings, because three containers disagree:

    - ID3 puts it in `UFID:http://musicbrainz.org`
    - Vorbis puts it in `musicbrainz_trackid`
    - MP4 uses a freeform `----:com.apple.iTunes:MusicBrainz Track Id` atom

    And one near-miss is excluded on purpose. `musicbrainz release track id`
    (ID3's `TXXX`) is the **track** id, unique to one release, and it sits
    beside the real one on every Picard-tagged file. Matching it is how a
    library ends up keyed by a per-release id that changes when the album does.
    """
    for key in tags:
        if key.startswith("ufid:") and "musicbrainz.org" in key:
            return True
        flat = key.replace(" ", "").replace("_", "")
        if "musicbrainz" in flat and "trackid" in flat and "releasetrackid" not in flat:
            return True
    return False


def lucene(artist: str, title: str) -> str:
    """The query MusicBrainz is asked, in one place so it cannot drift."""
    return f'recording:"{title}" AND artist:"{artist}"' if artist else f'recording:"{title}"'


def params_for(artist: str, title: str) -> dict[str, str]:
    """The request, as parameters."""
    return {"query": lucene(artist, title), "fmt": "json", "limit": str(CANDIDATES)}


def query_key(artist: str, title: str) -> str:
    """The cache key this question lands on, for reporting hits and misses."""
    return bronze.key_of(MB_RECORDING, params_for(artist, title))


def search(con: sqlite3.Connection, artist: str, title: str) -> dict[str, Any] | None:
    """Ask MusicBrainz for a recording, or recall having asked.

    **Keyed by the request, not by `(artist, title)`** — which is what this
    used and paid for twice in one afternoon. Normalising the query changed
    every key, so improving the question threw away every answer to the old
    one. Through `bronze` the key is the URL that was actually fetched, so a
    better query adds rows and the old ones stay readable (#58).
    """
    status, body = bronze.get(con, MB_RECORDING, params_for(artist, title), headers=USER_AGENT)
    return body if status == HTTP_OK and isinstance(body, dict) else None


def credit_of(recording: dict[str, Any]) -> str:
    """A candidate's full artist credit, joined the way the tags would write it."""
    names = [a["name"] for a in recording.get("artist-credit") or [] if isinstance(a, dict) and "name" in a]
    return ", ".join(names)


def best(candidates: list[dict[str, Any]], ours: float | None, who: str | None = None) -> dict[str, Any] | None:
    """The candidate to believe, which is not always the first one.

    **MusicBrainz ranks by name, and several candidates routinely tie at 100.**
    "All Night Long" is a dozen recordings of one title; the search cannot tell
    which is the four-minute edit and which is the nine-minute club mix,
    because nothing in the query said. Taking `recordings[0]` therefore picked
    arbitrarily among equals, and a mismatch was reported as *uncertain* — as
    though MusicBrainz did not have the track, when it had it in second place.

    Measured on the two roots before this existed: **108 of 191 uncertain rows
    had a lower-ranked candidate whose length agreed**, and every one of those
    was already in the cache. Not one uncertain row was a weak name match.

    So duration **selects** rather than merely grades: among the candidates
    MusicBrainz considers strong, take the one closest in length. With no
    duration to compare, the ranking is all there is and the first stands.
    """
    if not candidates or ours is None:
        return candidates[0] if candidates else None

    # **Length outranks the name ranking**, within reason. MusicBrainz scores by
    # title similarity, which cannot tell an album version from its radio edit —
    # both are the same title by the same artist. A candidate whose length
    # agrees to the second is the better answer even at a lower score, and
    # `PLAUSIBLE` is only there to stop that reasoning reaching a candidate
    # whose name is not really a match at all.
    #
    # A length of **zero means MusicBrainz has none recorded**, not a
    # zero-second recording. Seven candidate lists here contain one, and
    # comparing against it made a real recording look wrong by its whole
    # duration.
    plausible = [c for c in candidates if isinstance(c.get("score"), int) and c["score"] >= PLAUSIBLE]
    timed = [c for c in plausible if isinstance(c.get("length"), int) and c["length"] > 0]
    if not timed:
        strong = [c for c in candidates if isinstance(c.get("score"), int) and c["score"] >= STRONG]
        return strong[0] if strong else candidates[0]

    def drift(candidate: dict[str, Any]) -> float:
        length = candidate["length"]
        return abs(float(length) / 1000.0 - ours)

    # **A candidate naming the remixer wins outright** (#161). Duration cannot
    # separate two remixes of one track — `Slip Away (Larse Vox Remix)` is 518s
    # against our 517s — so where one candidate says whose remix it is and
    # fits the length, it is the answer and the ranking is beside the point.
    if who:
        claimed = [c for c in timed if names(c, who) and drift(c) <= SAME_RECORDING]
        if claimed:
            return min(claimed, key=drift)
    return min(timed, key=drift)


#: Dashes and quotes that two catalogues spell differently and mean
#: identically. Normalised away before any title is compared.
CONFUSABLES: Final = str.maketrans(
    {
        "\u2010": "-",
        "\u2011": "-",
        "\u2012": "-",
        "\u2013": "-",
        "\u2014": "-",
        "\u2018": "'",
        "\u2019": "'",
        "\u201c": '"',
        "\u201d": '"',
    }
)

#: Any bracketed group. What is *inside* it decides whether it matters.
BRACKETED: Final = re.compile(r"[([][^)\]]*[)\]]")

#: Words that name a **different version** of a song, as against the name of
#: whoever made it. "Original mix" is absent on purpose: it is Beatport for
#: "not a remix", and is stripped from the query already.
VERSION_WORDS: Final = re.compile(
    r"\b(?:instrumental|clean|acapella|a cappella|karaoke|live|demo|radio\s*edit|extended)\b"
)


#: A bracketed segment naming **some** version — any version at all. Wider
#: than `VERSION_WORDS`, which asks "is theirs a different version from ours";
#: this asks "does this title claim to be a version of something", and a title
#: that claims nothing is the candidate for being the thing itself.
A_VERSION: Final = re.compile(
    r"[([][^)\]]*\b(?:re-?mix|rework|refix|vip|bootleg|edit|instrumental|live"
    r"|acappella|a cappella|dub|version|mix)\b[^)\]]*[)\]]",
    re.IGNORECASE,
)

#: Beatport for "this is the thing itself". Kept apart from `A_VERSION`
#: because it is the one bracketed qualifier that marks an *original* rather
#: than a departure from one.
SAYS_ORIGINAL: Final = re.compile(r"[([]\s*original(?:\s+mix)?\s*[)\]]", re.IGNORECASE)


def original_among(candidates: list[dict[str, Any]], asked: str, chosen: dict[str, Any]) -> dict[str, Any] | None:
    """The recording a remix was made out of, where the answer is unambiguous.

    **The search already asked the right question.** `asked_about` strips the
    remix suffix and swaps in the original artist, so the result set is
    `recording:"<bare title>" AND artist:"<original artist>"` — which is the
    query for the original. It was in front of us the whole time and was being
    thrown away.

    So the original is picked out by what its title does *not* say: take the
    candidates carrying no version qualifier at all, or the ones explicitly
    marked `(original mix)` where any say so. Duration cannot help here — an
    original and its remix have no expected relationship in length — so the
    only honest signal is that exactly one candidate is left.

    **Ambiguity refuses.** Measured over the 56 confirmed remixes: 23 leave
    exactly one unqualified candidate, 9 leave none, and 24 leave several at
    different lengths — four `Slip Away` recordings between 331s and 388s,
    with no way to say which one Giacomotto remixed. Guessing among those is
    the sneaky rule this library would rather fail on and wait, so it does.

    Of the 23, **17 survive the two guards below** and become the first
    derivation edges the model has ever held.
    """
    pool = [c for c in candidates if SAYS_ORIGINAL.search(c.get("title") or "")] or [
        c for c in candidates if not A_VERSION.search(c.get("title") or "")
    ]
    if len(pool) != 1:
        return None
    it = pool[0]
    # **Not the remix itself.** `A_VERSION` catches every remix title seen so
    # far, but a remix whose suffix is a bare name — `Riot - Carl Cox` — would
    # slip through and derive a recording from itself. The model refuses that
    # too; refusing it here says why.
    if it.get("id") == chosen.get("id"):
        return None
    # **The title has to be the one asked for**, not merely the best answer
    # MusicBrainz could offer. A search that finds nothing still returns its
    # nearest misses, and a nearest miss ranked alone is still alone.
    if plain(BRACKETED.sub("", it.get("title") or "")) != plain(asked):
        return None
    # **A strong name, because nothing else here can corroborate.** An
    # identification has duration to check it; this has the title and the
    # artist and no third thing, so the one signal available is held high.
    # Costs 6 of the 23; they are the ones a person should read.
    return it if (it.get("score") or 0) >= STRONG else None


def plain(text: str) -> str:
    """A title with the spellings that carry no meaning taken out."""
    return re.sub(r"\s+", " ", text.translate(CONFUSABLES).lower()).strip()


def variance(ours: float | None, theirs: object) -> str:
    """What kind of not-a-match this is, when a qualifier says it is not one.

    **The guard stops producing doubt and starts producing evidence.** A
    version qualifier at the same length is mix-level variance — a channel
    muted, the song otherwise untouched — and belongs on the recording as a
    qualifier. A qualifier with a materially different length is a *derivation*:
    the recording was cut or rebuilt, which is a different fact with a different
    home (`RecordingDerivation`).

    Syntactically the two are identical — `(Instrumental)` and `(Radio Edit)`
    are one shape — so the length is the only thing that separates them, and
    that is why the two tolerances above must not be one.
    """
    if ours is None or not isinstance(theirs, int) or theirs <= 0:
        return "unknown"
    drift = abs(theirs / 1000.0 - ours)
    if drift <= SAME_MIX:
        return "qualifier"
    if max(ours, theirs / 1000.0) <= min(ours, theirs / 1000.0) * SIBLING_SPAN:
        return "derivation"
    return "unrelated"


def unasked_qualifier(ours: str, theirs: str) -> str | None:
    """A version qualifier on their title that ours never mentions.

    **A qualifier nobody asked for is evidence against the match**, and
    duration cannot see it: *212* and *212 (Instrumental)* are the same length
    by construction, so the vocal and the instrumental are indistinguishable to
    every check above this one. That match was made, and it was wrong.

    It is not a ranking problem either — the plain vocal was not among the
    three candidates MusicBrainz returned at all. Raising the limit is a
    different question with a real cost; refusing to be confident is free.

    **Compared by meaning, not by bytes.** A first version compared the
    bracketed text literally and downgraded nine good matches out of eleven:
    `Pan-Pot` against `Pan-Pot` differs only by U+2010, `Format:B` against
    `Format: B` by a space, and `Alors on danse Radio Edit` carries its
    qualifier with no brackets at all. So what is compared is the qualifying
    *word*, against the whole of our title, however either side spells it.

    A remixer's name is not a qualifier. `(Pan-Pot remix)` names who, not what,
    and the title already had to match for the candidate to get this far.

    This downgrades rather than rejects: the row still carries the candidate,
    marked `uncertain`, which is the honest direction to fail in.
    """
    mine = plain(ours)
    for group in BRACKETED.finditer(plain(theirs)):
        unasked = [w for w in VERSION_WORDS.findall(group.group(0)) if w not in mine]
        if unasked:
            return group.group(0)
    return None


def names(recording: dict[str, Any], who: str | None) -> bool:
    """Whether a candidate's own title credits this remixer — #161.

    The corroboration duration cannot give. Two remixes of one track are
    routinely the same length: `Slip Away (Larse Vox Remix)` is 518s and our
    Giacomotto remix is 517s, so a rule that accepted the closest duration
    would file one as the other.
    """
    if not who:
        return False
    return plain(who) in plain(str(recording.get("title") or ""))


def contested(candidates: list[dict[str, Any]], ours: float | None, who: str | None) -> bool:
    """Whether several candidates fit the duration and none names the remixer.

    **The `Larse Vox` trap.** With a remixer to look for and nobody claiming to
    be them, two candidates a second apart are two remixes and the length says
    nothing about which is ours. One candidate is a different matter: there is
    nothing to confuse it with.
    """
    if not who or ours is None:
        return False
    near = [
        c
        for c in candidates
        if isinstance(c.get("length"), int) and c["length"] > 0 and abs(c["length"] / 1000.0 - ours) <= SAME_RECORDING
    ]
    return len(near) > 1 and not any(names(c, who) for c in near)


@dataclass(frozen=True)
class Answer:
    """One search: what it asked, and everything that came back.

    **The losing candidates are part of the answer**, which is why this is a
    record rather than the winner alone. `best` picks one and that is the
    identification, but the original a remix was made out of is a *different*
    row of the same result set (#162) — so a pass that keeps only `chosen`
    throws away the evidence for half of what it learned.
    """

    #: The title actually put to MusicBrainz — the remix suffix already gone.
    asked: str
    #: The title as the library holds it, suffix and all. `asked` cannot stand
    #: in for it: the suffix is exactly what says whether a person is credited.
    said: str
    #: Who the title said remixed it, where it said so.
    remixer: str | None
    #: Everything MusicBrainz offered, in its order.
    candidates: list[dict[str, Any]]
    #: The one `best` believes.
    chosen: dict[str, Any]


def also(row: Work, answer: Answer, confidence: str) -> list[dict[str, Any]]:
    """Everything else a confident match established — #162.

    The identification says *which recording this is*. Two more things were
    known at the same moment and were being discarded:

    - **who remixed it**, where the candidate's own title corroborates the
      remixer our title named — evidence no tag on its own provides;
    - **what it was made out of** — the original is in the same result set,
      because the search is the bare title against the original artist. That is
      the first thing ever to mint a `RecordingDerivation`.

    **The version qualifier is not here**, and that is not an omission:
    `Instance::ingest` already reads `qualifiers_in` off the title and asserts
    it at `TagVariant`. Adding `original` to that vocabulary is the whole of
    what #162 needed on that side, and emitting it again from here would be a
    second answer to one question.

    **A cover is deliberately not expressible.** It realises the work rather
    than being made out of the recording, and belongs one level up.

    Nothing is emitted for a match a person still has to read: an uncertain
    identification is not evidence of a remix relationship.
    """
    remixer = answer.remixer
    if confidence not in APPLIES or not remixer or not names(answer.chosen, remixer):
        return []
    # **Nothing to hang a credit on yet** — #160. A row the library has not
    # named has no recording, and both facts below are *about* a recording.
    # The identification on the line above mints one, but its id is
    # `of_basis(MusicBrainz(...))` and computing that here would be a second
    # door onto identity (`layers.md` §5: a tool cannot mint an id).
    #
    # Not lost: once the identification applies the row is catalogued like any
    # other, and `identify derivations` reads it off the ordinary work list.
    if row.recording is None:
        return []
    # **A bare `mix` names a version, not a person** — see `CREDITS_A_PERSON`.
    # This is the rung between shaping a question and asserting an answer, and
    # it is the only place the distinction matters.
    if not CREDITS_A_PERSON.search(answer.said):
        return []
    lines: list[dict[str, Any]] = [
        {
            # **`contributed`, not `credited`** — the latter is the *split*,
            # which refuses a single name and asserts `performer`. A remixer
            # must not be a performer: `line_of` reads performers, so the wrong
            # role would put the record in the remixer's folder.
            "fact": "contributed",
            "recording": row.recording,
            "artist": remixer,
            "role": "remixer",
            # **MusicBrainz's claim, not the tag's.** Our title named this
            # remixer and the candidate's own title agrees; that agreement is
            # the whole reason this line is written.
            "basis": "musicbrainz",
        }
    ]
    origin = original_among(answer.candidates, answer.asked, answer.chosen)
    if origin:
        lines.append(
            {
                "fact": "recording_derived",
                # **A basis, not an id.** The original is a fact about the
                # world and the library need not hold a copy of it — the whole
                # point of the archivist reading (`library-api.md` §1). Naming
                # it by its MusicBrainz id lets the edge stand whether or not
                # the file ever arrives, and mints the same id if it does.
                "from": {"musicbrainz": origin.get("id")},
                # **The row as it stands now**, which the identification on the
                # line above is about to merge into its MusicBrainz twin.
                # References resolve through redirects when read, so naming the
                # id that exists at apply time is the same answer as naming the
                # one that survives (`merge.rs`, "nothing is destroyed").
                "to": row.recording,
                "kind": "Remix",
                "why": f"{origin.get('title')!r} is the only unqualified candidate, score {origin.get('score')}",
            }
        )
    return lines


def graded(answer: Answer, seconds: float | None) -> tuple[str, str, str | None]:
    """How much to believe this match, why, and what kind of difference remains.

    Three checks in order, and each can only *lower* the verdict — which is the
    property that makes them safe to compose. `verdict` grades the name and the
    duration; the two below are the things those two cannot see.
    """
    confidence, why = verdict(answer.chosen, seconds)
    remixer = answer.remixer
    # **A remix nobody claims, among candidates the length cannot separate**
    # (#161). Two remixes of one track are routinely the same length; where
    # several fit and none says whose remix it is, the duration has decided
    # nothing and a person should.
    if confidence == "matched" and contested(answer.candidates, seconds, remixer):
        confidence = "uncertain"
        why = f"{why}, but nothing here says it is the {remixer} remix and more than one fits"
    elif confidence == "matched" and names(answer.chosen, remixer):
        why = f"{why}, and it names the {remixer} remix"
    extra = unasked_qualifier(answer.said, str(answer.chosen.get("title") or ""))
    if confidence == "matched" and extra:
        return "uncertain", f"{why}, but theirs is {extra}", variance(seconds, answer.chosen.get("length"))
    return confidence, why, None


def verdict(recording: dict[str, Any], ours: float | None) -> tuple[str, str]:
    """How much to believe this candidate, and why.

    Three levels, and the middle one is the point: a strong *name* match whose
    duration cannot be checked is not the same evidence as one whose duration
    agrees, and collapsing them is how a live take gets filed as the album cut.
    """
    score = recording.get("score", 0)
    if not isinstance(score, int) or score < PLAUSIBLE:
        return "uncertain", f"name score {score}"

    length = recording.get("length")
    # Zero is "MusicBrainz records no length", not a zero-second recording.
    known = isinstance(length, int) and length > 0
    if ours is None or not known:
        if score < STRONG:
            return "uncertain", f"name score {score}, and no duration to check it with"
        return "probable", f"name score {score}, no duration to check"

    drift = abs(length / 1000.0 - ours)
    if drift <= SAME_RECORDING:
        # **A length that agrees is the stronger evidence.** Two recordings of
        # one title by one artist are indistinguishable by name; agreeing to the
        # second is what separates the album version from its radio edit.
        return "matched", f"name score {score}, duration within {drift:.1f}s"
    if score < STRONG:
        return "uncertain", f"name score {score} and duration off by {drift:.0f}s"
    return "uncertain", f"name score {score} but duration off by {drift:.0f}s"


@dataclass(frozen=True)
class Work:
    """One row of `gmuse library query --unidentified`.

    **The id is what a proposal names; the locator is what a pass opens.** They
    have deliberately different lifetimes — the locator is good until the file
    moves, the id is good forever — and keeping them apart is what stops a tool
    from naming its findings by path.

    This pass used to walk the disk and emit `{"locator": ...}`, which is why
    712 confident matches sat in `tools/out/` that `apply` could not read: a
    path is a property of one machine's copy and names nothing the library
    knows.

    **A row with no recording is the ordinary case now** — #160. The import
    gate refuses to mint a performance for a file no authority names, so what
    the library holds is bytes and a claim; the row carries an `encoding` and a
    `locator` and nothing else to name. That is exactly enough, because an
    identification names the encoding either way.
    """

    #: `None` for a row the library has not named. The proposal never used
    #: this — `identified` names the encoding — but `also` does, and cannot
    #: speak about a performance that does not exist yet.
    recording: str | None
    encoding: str | None
    locator: str | None
    title: str
    artist: str


def work_list(path: Path, *, limit: int | None = None) -> list[Work]:
    """Read what gmuse handed us, keeping only rows this machine can open.

    A row with no locator is an ordinary row — something the library knows
    about and this disk does not hold — and there is nothing for a pass that
    reads audio to do with one.
    """
    rows = []
    for raw_line in path.read_text(encoding="utf-8").splitlines():
        line = raw_line.strip()
        if not line or line.startswith("#"):
            continue
        raw = json.loads(line)
        row = Work(
            recording=raw.get("recording"),
            encoding=raw.get("encoding"),
            locator=raw.get("locator"),
            title=raw.get("title", ""),
            artist=raw.get("artist", ""),
        )
        if row.locator and row.encoding:
            rows.append(row)
    return rows[:limit] if limit is not None else rows


def unreadable(todo: list[tuple[Path, dict[str, str]]]) -> str | None:
    """Why this batch should not be sent, or None if it is fine.

    **A guard, because this failure has happened twice and is silent both
    times.** A tag reader that returns nothing does not raise; it queries
    MusicBrainz with an empty artist and a filename, gets no match, and reports
    that MusicBrainz has none of your music. The first time it was a query the
    `artist:` field could not match; the second, mutagen's raw ID3 frame ids
    where plain names were expected. Both read as a gap in the source rather
    than a gap in the code, which is what makes them expensive: 591 requests at
    one a second, spent learning nothing.

    A library where *nothing* has an artist is not a real library. One where
    some files do and some do not is ordinary, and passes.
    """
    if not todo:
        return None
    if not any(tags.get("artist") for _, tags in todo):
        return f"not one of {len(todo)} files has a readable artist — the tag reader is broken"
    return None


def fetch(work: Path, *, limit: int | None) -> None:
    """Look up every recording in the work list that no authority has named.

    The list comes from `gmuse library query --unidentified`, so which files
    need looking up is the library's answer rather than this pass's guess —
    and the two cannot disagree about it.
    """
    con = bronze.open_cache()
    rows = work_list(work, limit=limit)
    todo = [(Path(row.locator or ""), read(Path(row.locator or ""))[0]) for row in rows]

    print(f"{len(rows)} recordings to look up, from {work}")
    if problem := unreadable(todo):
        print(f"refusing to ask MusicBrainz: {problem}")
        return
    asked = cached = 0
    for index, (path, tags) in enumerate(todo, start=1):
        artist, title, _ = asked_about(tags, path.stem)
        if not title:
            continue
        before = con.execute("SELECT 1 FROM response WHERE key = ?", (query_key(artist, title),)).fetchone()
        search(con, artist, title)
        if before is None:
            asked += 1
        else:
            cached += 1
        if index % 25 == 0:
            print(f"  {index}/{len(todo)}  ({asked} asked, {cached} already cached)", flush=True)
    print(f"done: {asked} asked, {cached} already cached")


def derivations(work: Path, out: Path, *, limit: int | None) -> None:
    """What the *already identified* remixes established and nobody kept — #162.

    `propose` learns three things at once and `also` writes all three, but it
    only ever runs over rows gmuse calls **unidentified**. Every remix the
    library had already settled — 249 of them, named as a remix by their own
    title — was identified before `also` existed, so its remixer credit and the
    original it was made out of were known once and thrown away.

    **This does not re-identify anything**, and that is the whole reason it is
    a separate command rather than a flag. These rows already rest on a
    MusicBrainz basis; proposing `identified` over them again would relink
    MusicBrainz to MusicBrainz, which does not merge and strands whatever the
    old id carried — 24 descriptions, once, and that is
    `identification-lessons.md` §10. So the identification line is not written
    here at all. The search is run for its *evidence*, and only `also`'s
    findings are kept.

    **The library's title, not the file's.** A settled row has a name the
    library stands behind; reaching past it to the tag would ask MusicBrainz
    about a string the library already decided was wrong. The file is opened
    for its duration alone, which is a fact about bytes and is not in the log.
    """
    con = bronze.open_cache()
    counts = {"derived": 0, "credited": 0, "unsure": 0, "no candidates": 0}
    with out.open("w", encoding="utf-8") as sink:
        for row in work_list(work, limit=limit):
            # **Skip what the library has not named** — #160. This command is
            # for rows already settled against MusicBrainz; a held-back row has
            # no recording, so `also` can say nothing about it and the search
            # would spend a rate limit to be told so.
            if row.recording is None:
                continue
            said = {"title": row.title, "artist": row.artist}
            artist, title, remixer = asked_about(said, "")
            if not remixer or not title:
                continue
            _, seconds = read(Path(row.locator or ""))
            body = search(con, artist, title)
            candidates = (body or {}).get("recordings") or []
            chosen = best(candidates, seconds, remixer) if candidates else None
            if chosen is None:
                counts["no candidates"] += 1
                continue
            confidence, _ = verdict(chosen, seconds)
            if confidence == "matched" and contested(candidates, seconds, remixer):
                confidence = "uncertain"
            lines = also(row, Answer(title, row.title, remixer, candidates, chosen), confidence)
            if not lines:
                counts["unsure"] += 1
                continue
            for line in lines:
                counts["derived" if line["fact"] == "recording_derived" else "credited"] += 1
                sink.write(json.dumps({**line, "ours": {"title": row.title, "artist": row.artist}}) + "\n")
    print(f"{counts['derived']} originals and {counts['credited']} remixer credits -> {out}")
    print(f"  {counts['unsure']} said nothing new, {counts['no candidates']} had no candidate at all")


def propose(work: Path, out: Path, *, limit: int | None) -> None:
    """Turn the cache into a file `gmuse library apply` can read.

    **One file, two readers.** Every line carries the proposal fields gmuse
    parses *and* the evidence a person needs to judge it — the parser ignores
    fields it does not know, so the review copy and the machine copy are the
    same file rather than two that drift apart.

    A row this pass is not confident about is written **commented out**. The
    parser skips `#` lines, so an uncertain match cannot apply by accident, and
    accepting one is deleting a `#`. That is the review step, and it is an edit
    a person can make in the file they are already reading.
    """
    con = bronze.open_cache()
    todo = []
    for row in work_list(work, limit=limit):
        path = Path(row.locator or "")
        tags, seconds = read(path)
        todo.append((row, path, tags, seconds))

    counts = {"matched": 0, "probable": 0, "uncertain": 0, "nothing": 0}
    held_back = sum(1 for row, *_ in todo if row.recording is None)
    with out.open("w", encoding="utf-8") as sink:
        for row, path, tags, seconds in todo:
            artist, title, remixer = asked_about(tags, path.stem)
            body = search(con, artist, title) if title else None
            candidates = (body or {}).get("recordings") or []
            if not candidates:
                counts["nothing"] += 1
                continue
            chosen = best(candidates, seconds, remixer)
            if chosen is None:
                counts["nothing"] += 1
                continue
            confidence, why, kind = graded(
                Answer(title, tags.get("title", "") or path.stem, remixer, candidates, chosen),
                seconds,
            )
            counts[confidence] += 1
            # Confident rows apply; the rest are commented out, so accepting
            # one is deleting a `#` in the file being reviewed.
            prefix = "" if confidence in APPLIES else "# "
            sink.write(
                prefix
                + json.dumps(
                    {
                        "fact": "identified",
                        "encoding": row.encoding,
                        "basis": {"musicbrainz": chosen.get("id")},
                        # Everything below is for the person reading. The
                        # parser ignores fields it does not know, which is what
                        # lets one file be both.
                        "locator": str(path),
                        "confidence": confidence,
                        "why": why,
                        # What kind of difference this is, when the guard
                        # refused. `qualifier` and `derivation` have different
                        # homes in the model (#101), and only the length can
                        # tell them apart.
                        **({"variance": kind} if kind else {}),
                        "queried": {"artist": artist, "title": title},
                        "theirs": {"title": chosen.get("title"), "artist": credit_of(chosen)},
                        "ours": {"title": tags.get("title", ""), "artist": tags.get("artist", "")},
                    }
                )
                + "\n"
            )
            # **What else this match established** — #162. The identification
            # is one fact; the remix that produced it is three, and until now
            # all three were thrown away at the moment they were known.
            said = tags.get("title", "") or path.stem
            for line in also(row, Answer(title, said, remixer, candidates, chosen), confidence):
                sink.write(prefix + json.dumps(line) + "\n")

    total = sum(counts.values())
    applies = sum(counts[level] for level in APPLIES)
    print(f"{total} recordings looked up -> {out}")
    print(f"  {applies} would apply; the rest are commented out")
    # **How many of these the library never named** — #160. The import gate
    # holds a file's bytes without minting a performance, so these rows arrive
    # here with an encoding and no recording. Worth counting separately: it is
    # the population the gate created, and a matched one is catalogued by this
    # proposal rather than merely relinked by it.
    if held_back:
        print(f"  {held_back} of them the library holds and has never named")
    for level in ("matched", "probable", "uncertain", "nothing"):
        share = f"{100 * counts[level] / total:.1f}%" if total else "-"
        print(f"  {level:9s} {counts[level]:5d}  {share}")
    print("\nreview the commented-out ones; delete the `#` to accept one:")
    print(f"  grep '^# ' {out} | head")
    print(f"\nthen: gmuse library apply {out}       # dry run")
    print(f"      gmuse library apply {out} --write")


WORK = typer.Argument(help="the work list from `gmuse library query --unidentified`")
LIMIT = typer.Option(None, help="stop after this many rows")
OUT = typer.Option(None, help="where to write the proposals")


#: Where a fingerprint is looked up. The client key is a `SECRET_PARAMS`
#: member in `bronze`, so it never reaches a cache key — rotating it is not a
#: cache flush, and the fingerprint *is* the question being asked.
ACOUSTID: Final = "https://api.acoustid.org/v2/lookup"

#: Chromaprint's command-line tool. Present already — the fingerprint
#: experiments in #57 were run with it before there was a key to look one up.
FPCALC: Final = "fpcalc"

#: An AcoustID match score at or above this is the audio, not a near miss.
#:
#: Measured over the first 24 unidentified files with a copy here: every result
#: that carried a MusicBrainz recording scored **0.76 or better**, and the eight
#: of them were all correct. Nothing landed in between, so this is set at the
#: bottom of the observed band rather than at a number chosen for looking safe.
SURE: Final = 0.70

#: How alike two titles must read before a crowd-sourced link is believed.
#:
#: Chosen against the measurement in `corroborates`: at 0.72 every wrong match
#: in the first run is refused and every right one that differs only by tag
#: noise survives. It is deliberately loose — `Mr. Bass feat. Super Flu
#: (Original Mix)` and `Mr. Bass` have to pass.
ALIKE: Final = 0.72


def fingerprinted(path: Path) -> tuple[str, int] | None:
    """What this file sounds like, as chromaprint hears it.

    **Not cached, and it does not need to be.** The expensive half of this pass
    is the lookup, and `bronze` caches that keyed by the fingerprint — so a
    re-run pays `fpcalc` again (about a third of a second a file, a minute for
    the whole work list) and pays the network nothing.
    """
    out = subprocess.run([FPCALC, "-json", str(path)], capture_output=True, text=True, check=False).stdout
    try:
        d = json.loads(out)
        return d["fingerprint"], int(d["duration"])
    except (json.JSONDecodeError, KeyError, TypeError, ValueError):
        return None


def heard(con: sqlite3.Connection, key: str, fingerprint: str, seconds: int) -> list[dict[str, Any]]:
    """Every recording AcoustID links to this audio, best score first."""
    status, body = bronze.get(
        con,
        ACOUSTID,
        {
            "client": key,
            "duration": str(seconds),
            "fingerprint": fingerprint,
            "meta": "recordings",
            "format": "json",
        },
        headers=USER_AGENT,
    )
    if status != HTTP_OK or not isinstance(body, dict):
        return []
    found: list[dict[str, Any]] = [r for r in (body.get("results") or []) if r.get("recordings")]

    def strength(result: dict[str, Any]) -> float:
        return -float(result.get("score") or 0)

    return sorted(found, key=strength)


def sounds_like(result: dict[str, Any], seconds: float | None) -> dict[str, Any] | None:
    """The one recording to believe, among those on a single AcoustID.

    **Duration decides, and it is the only thing that can.** One AcoustID is one
    piece of audio, so where it links several MusicBrainz recordings they are
    usually one performance entered twice — but not always, and the title cannot
    separate them here for the reason given on `credited_by_sound`.
    """
    recordings: list[dict[str, Any]] = result.get("recordings") or []
    if len(recordings) == 1:
        return recordings[0]
    if seconds is None:
        return None
    close = [
        # **A float**, and this cost the corroborator once: AcoustID answers
        # `560.0`, an `isinstance(..., int)` guard is False for it, and every
        # row of the first run read "no duration to check".
        r
        for r in recordings
        if isinstance(r.get("duration"), (int, float)) and abs(r["duration"] - seconds) <= SAME_RECORDING
    ]
    return close[0] if len(close) == 1 else None


def folded(text: str) -> str:
    """A name reduced to what two spellings of it can be expected to share.

    Accents are **folded away**, not stripped: `Lvis Mejía` reaches this pass
    as `Lvis Meji` + U+00B4 + `a`, because some tagger long ago turned a combining acute
    into a literal one. Removing every non-ASCII character maps those two to
    `lvismejia` and `lvismeja`, which then do not match — and the first version
    of this check refused both of that artist's tracks for exactly that reason.
    """
    decomposed = unicodedata.normalize("NFKD", text or "")
    without_marks = "".join(c for c in decomposed if not unicodedata.combining(c))
    return re.sub(r"[^a-z0-9]", "", without_marks.lower())


def named_in(credit: str) -> set[str]:
    """Every artist a credit string names, folded."""
    parts = re.split(r"[,;/&]| feat\.? | ft\.? | vs\.? | and ", credit or "", flags=re.IGNORECASE)
    return {folded(p) for p in parts if folded(p)}


def corroborates(ours: dict[str, str], theirs: dict[str, str]) -> bool:
    """Whether the names agree well enough to believe a crowd-sourced link.

    **A fingerprint is not self-corroborating**, and building this pass without
    that was the mistake worth recording. `score` measures how closely the audio
    matches a fingerprint AcoustID has stored; it says nothing about whether the
    MusicBrainz id somebody attached to that fingerprint is right. The links are
    submitted by whoever ran a tagger, and a wrong submission is permanent.

    Measured over the first 203 files: **51 matches confident on score and
    duration alone, of which roughly 15 were wrong** — `Motorcity` to a funeral
    doom record, a Giacca remix to `Roll Over Beethoven`, a Giacomotto remix to
    a Lexer one, all at score 1.00.

    So the names come back as the check — and this is the *opposite* of the
    rule on the name path, for a reason worth stating. There the search is *by*
    name, so name agreement is circular: it says the search worked, not that
    the record is right, and duration is the independent axis. Here the search
    is by audio, so the names are genuinely independent evidence about the link.

    Loose on purpose, because tag noise is exactly what this pass exists to see
    through: an artist need only **share one name** (`Andhim, Super Flu` against
    `Andhim`), and a title need only contain the other or be close to it
    (`Mr. Bass feat. Super Flu (Original Mix)` against `Mr. Bass`). What it
    refuses is two records with nothing in common.
    """
    mine, yours = named_in(ours.get("artist", "")), named_in(theirs.get("artist", ""))
    shared = bool(mine & yours) or any(a in b or b in a for a in mine for b in yours)
    if not shared:
        return False
    x, y = folded(ours.get("title", "")), folded(theirs.get("title", ""))
    if not x or not y:
        return False
    return x in y or y in x or SequenceMatcher(None, x, y).ratio() >= ALIKE


def credited_by_sound(recording: dict[str, Any]) -> str:
    """A candidate's artists, joined as a tag would write them."""
    return ", ".join(a["name"] for a in recording.get("artists") or [] if "name" in a)


def listened(
    con: sqlite3.Connection, key: str, path: Path, seconds: float | None
) -> tuple[dict[str, Any] | None, dict[str, Any] | None, str]:
    """Fingerprint this file and ask, returning the result, the recording, or why not.

    Three ways to get nothing, and they are different answers rather than one
    failure: the file would not decode, AcoustID has never heard it or has
    nothing linked to it, or one AcoustID names several recordings the length
    cannot separate.
    """
    printed = fingerprinted(path)
    if not printed:
        return None, None, "fpcalc could not read it"
    results = heard(con, key, printed[0], printed[1])
    if not results:
        return None, None, "nothing MusicBrainz has linked"
    best = results[0]
    chosen = sounds_like(best, seconds)
    if chosen is None:
        return best, None, "several recordings, and the length cannot separate them"
    return best, chosen, ""


def by_sound(work: Path, out: Path, *, limit: int | None) -> None:
    """Identify by what the file sounds like — #57, and the rung #160 was missing.

    The ladder `library-api.md` §7 describes runs `MusicBrainz id → fingerprint
    → recording search → refuse`. Everything but the middle rung existed; this
    is the middle rung.

    **The title check is off here, and that is the whole point.** `propose`
    downgrades a match whose title carries something ours does not, because on
    a *name* search a surprise in the title means the search found the wrong
    record. On a fingerprint it means the opposite: the audio already matched,
    so a differing title is the tag being wrong, which is the case this exists
    to catch. Measured on the first 24 files — every one of these would have
    been thrown away by a title check, and every one is right:

    | ours | MusicBrainz |
    | --- | --- |
    | `Trixi B2` | `Trixi 1` |
    | `Control of Your Mind (vocal mix)` | `Control Your Mind (vocal mix)` |
    | `Andhim,Super Flu — Mr. Bass feat. Super Flu` | `Andhim — Mr. Bass` |
    | `Lvis Meji` U+00B4 `a` | `Lvis Mejía` |

    **So duration is the corroboration**, as it is everywhere else here: the
    fingerprint says which audio, the length says the library holds that much
    of it, and nothing else in the response is independent of the match.

    **A fingerprint AcoustID knows with no MusicBrainz recording behind it is
    not a failure to look up.** It is a different answer — the audio is
    catalogued and nobody has linked it — and it is two thirds of this work
    list, so it is counted apart from "never heard of it" rather than folded in
    with it.
    """
    key = secret("ACOUSTID_KEY")
    if not key:
        print("no AcoustID key: register at https://acoustid.org/new-application, then")
        print('  security add-generic-password -a "$USER" -s gmuse-acoustid -w')
        return
    con = bronze.open_cache()
    counts = collections.Counter()
    renamed = 0
    with out.open("w", encoding="utf-8") as sink:
        for row in work_list(work, limit=limit):
            path = Path(row.locator or "")
            if not path.exists():
                counts["no copy on this device"] += 1
                continue
            _, seconds = read(path)
            best, chosen, why_not = listened(con, key, path, seconds)
            if chosen is None or best is None:
                counts[why_not] += 1
                continue
            score = best.get("score") or 0
            theirs = chosen.get("duration")
            drift = abs(theirs - seconds) if isinstance(theirs, (int, float)) and seconds else None
            ours = {"title": row.title, "artist": row.artist}
            theirs = {"title": chosen.get("title") or "", "artist": credited_by_sound(chosen)}
            # **Three checks, and the third is the one that was missing.** The
            # score says the audio matches something AcoustID has stored; the
            # duration says we hold that much of it; the names say the id
            # somebody attached to that fingerprint is plausible. Without the
            # third, 51 matches were confident and about 15 of them were wrong.
            agrees = corroborates(ours, theirs)
            sure = score >= SURE and (drift is None or drift <= SAME_RECORDING) and agrees
            if not sure and score >= SURE and not agrees:
                counts["the link says a different record"] += 1
            else:
                counts["identified" if sure else "for a person to read"] += 1
            sink.write(
                ("" if sure else "# ")
                + json.dumps(
                    {
                        "fact": "identified",
                        "encoding": row.encoding,
                        "basis": {"musicbrainz": chosen.get("id")},
                        "locator": str(path),
                        "confidence": "matched" if sure else "uncertain",
                        "why": f"fingerprint score {score:.2f}"
                        + (f", duration within {drift:.0f}s" if drift is not None else ", no duration to check")
                        + ("" if agrees else ", but the names have nothing in common"),
                        "acoustid": best.get("id"),
                        "theirs": theirs,
                        "ours": ours,
                    }
                )
                + "\n"
            )
            # **And what it is called** — a `retitled` beside the
            # identification, where the two names differ by more than nothing.
            #
            # **Only on this path.** A name search found the record *by* its
            # title, so the titles agree by construction and a retitle would
            # say nothing. A fingerprint found it by audio, so a differing
            # title is a thing learned — and on a transposed pair it is the
            # whole finding: `The Bells Sketch` and `Give a Man a Rod` were
            # identified correctly and stayed labelled wrongly, because a
            # relink moves the binding and a merge carries the old title.
            # **Where there is a row to rename.** A held-back file has no
            # recording yet (#160); the identification above mints one and
            # catalogues it from the claim, so its title is still the tag's.
            # Renaming that wants the id the identification is about to mint,
            # which a tool must not compute — so it waits for the next pass,
            # when the row is ordinary.
            if row.recording and folded(ours["title"]) != folded(theirs["title"]):
                sink.write(
                    ("" if sure else "# ")
                    + json.dumps(
                        {
                            "fact": "retitled",
                            "recording": row.recording,
                            "title": theirs["title"],
                            "was": ours["title"],
                        }
                    )
                    + "\n"
                )
                renamed += 1
    total = sum(counts.values())
    print(f"{total} file(s) listened to -> {out}")
    for k, n in counts.most_common():
        print(f"  {n:4d}  {k}")
    if renamed:
        print(f"\n  {renamed} of them are also called something else by MusicBrainz")


def existing(root: Path) -> Path:
    """A path that is there, or a clean exit saying which is not."""
    root = root.expanduser()
    if not root.exists():
        typer.echo(f"no such root: {root}")
        raise typer.Exit(1)
    return root


def register(app: typer.Typer) -> None:
    """Attach this pass's commands to the tool."""

    @app.command("fetch")
    def fetch_cmd(work: Path = WORK, limit: int | None = LIMIT) -> None:
        """Ask MusicBrainz about the recordings gmuse says are unidentified."""
        fetch(existing(work), limit=limit)

    @app.command("derivations")
    def derivations_cmd(
        work: Path = WORK,
        out: Path | None = OUT,
        limit: int | None = LIMIT,
    ) -> None:
        """What the already-identified remixes established: a remixer, and an original."""
        work = existing(work)
        default = out_path(f"derivations-{work.stem.lower()}.jsonl")
        derivations(work, out.expanduser() if out else default, limit=limit)

    @app.command("fingerprint")
    def fingerprint_cmd(
        work: Path = WORK,
        out: Path | None = OUT,
        limit: int | None = LIMIT,
    ) -> None:
        """Identify by what the file sounds like, not by what its tags claim."""
        work = existing(work)
        default = out_path(f"fingerprint-{work.stem.lower()}.jsonl")
        by_sound(work, out.expanduser() if out else default, limit=limit)

    @app.command("propose")
    def propose_cmd(
        work: Path = WORK,
        out: Path | None = OUT,
        limit: int | None = LIMIT,
    ) -> None:
        """Turn the cache into a file `gmuse library apply` can read."""
        work = existing(work)
        default = out_path(f"identify-{work.stem.lower()}.jsonl")
        propose(work, out.expanduser() if out else default, limit=limit)
