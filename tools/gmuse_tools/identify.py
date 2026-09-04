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

import json
import re
from dataclasses import dataclass
from pathlib import Path
from typing import TYPE_CHECKING, Any, Final

import mutagen
import typer

from gmuse_tools import bronze
from gmuse_tools.genre.common import out_path

if TYPE_CHECKING:
    import sqlite3

USER_AGENT: Final = {"User-Agent": "gmuse-identify/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}
MB_RECORDING: Final = "https://musicbrainz.org/ws/2/recording"
HTTP_OK: Final = 200
#: How many candidates to keep. Past the third, a match is not one to trust.
CANDIDATES: Final = 3
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


def best(candidates: list[dict[str, Any]], ours: float | None) -> dict[str, Any] | None:
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
    """

    recording: str
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
            recording=raw["recording"],
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
        artist = first_artist(tags.get("artist", ""))
        title = searchable_title(tags.get("title", "") or path.stem)
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
    with out.open("w", encoding="utf-8") as sink:
        for row, path, tags, seconds in todo:
            artist = first_artist(tags.get("artist", ""))
            title = searchable_title(tags.get("title", "") or path.stem)
            body = search(con, artist, title) if title else None
            candidates = (body or {}).get("recordings") or []
            if not candidates:
                counts["nothing"] += 1
                continue
            chosen = best(candidates, seconds)
            if chosen is None:
                counts["nothing"] += 1
                continue
            confidence, why = verdict(chosen, seconds)
            kind: str | None = None
            extra = unasked_qualifier(tags.get("title", ""), str(chosen.get("title") or ""))
            if confidence == "matched" and extra:
                confidence, why = "uncertain", f"{why}, but theirs is {extra}"
                kind = variance(seconds, chosen.get("length"))
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

    total = sum(counts.values())
    applies = sum(counts[level] for level in APPLIES)
    print(f"{total} recordings looked up -> {out}")
    print(f"  {applies} would apply; the rest are commented out")
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
