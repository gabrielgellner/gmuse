#!/usr/bin/env python3
"""Find the MusicBrainz recording id for files that do not carry one.

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

import argparse
import datetime as dt
import json
import re
import sqlite3
import sys
import time
from pathlib import Path
from typing import Any, Final

import mutagen
import requests

from genre.common import out_path, walk

USER_AGENT: Final = {"User-Agent": "gmuse-identify/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}
MB_RECORDING: Final = "https://musicbrainz.org/ws/2/recording"
#: MusicBrainz asks for no more than one request a second, and means it.
COURTESY_DELAY: Final = 1.05
TIMEOUT: Final = 30
HTTP_OK: Final = 200
HTTP_BUSY: Final = 503
RETRIES: Final = 4
#: How many candidates to keep. Past the third, a match is not one to trust.
CANDIDATES: Final = 3
#: A Lucene score at or above this is a name match worth considering. Below it
#: MusicBrainz is guessing from a partial term.
STRONG: Final = 95
#: Seconds of disagreement still counted as the same recording. Encoder padding
#: and a trimmed lead-in account for one or two; five is generous and still
#: rules out the live take.
DURATION_SLACK: Final = 5.0

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
    raw = {str(key).lower(): str(value) for key, value in dict(handle.tags or {}).items()}
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


def open_cache(path: Path) -> sqlite3.Connection:
    """Open the lookup cache, creating it if this is the first run."""
    con = sqlite3.connect(path)
    con.execute(
        "CREATE TABLE IF NOT EXISTS search "
        "(artist TEXT, title TEXT, fetched_at TEXT, payload TEXT, PRIMARY KEY (artist, title))"
    )
    con.commit()
    return con


def get_json(params: dict[str, str]) -> dict[str, Any] | None:
    """One request, retried while the server says it is busy.

    **A 503 is not an answer.** The genre pass learned this the expensive way:
    treating every non-200 as "no data" cached a release *with* genres as
    having none, and the run printed a dash that read as a gap in MusicBrainz
    rather than as a throttle we caused.
    """
    delay = COURTESY_DELAY
    for _ in range(RETRIES):
        response = requests.get(MB_RECORDING, headers=USER_AGENT, timeout=TIMEOUT, params=params)
        time.sleep(delay)
        if response.status_code == HTTP_OK:
            body = response.json()
            return body if isinstance(body, dict) else None
        if response.status_code != HTTP_BUSY:
            return None
        delay *= 2
    return None


def search(con: sqlite3.Connection, artist: str, title: str) -> dict[str, Any] | None:
    """Ask MusicBrainz for a recording, or return what it said last time.

    The whole response is cached, not the conclusion drawn from it. Scoring
    changes; a network round trip at one per second does not come back.
    """
    row = con.execute("SELECT payload FROM search WHERE artist = ? AND title = ?", (artist, title)).fetchone()
    if row is not None:
        payload = json.loads(row[0])
        return payload if isinstance(payload, dict) else None

    query = f'recording:"{title}" AND artist:"{artist}"' if artist else f'recording:"{title}"'
    body = get_json({"query": query, "fmt": "json", "limit": str(CANDIDATES)})
    if body is None:
        return None
    con.execute(
        "INSERT OR REPLACE INTO search VALUES (?, ?, ?, ?)",
        (artist, title, dt.datetime.now(tz=dt.UTC).isoformat(), json.dumps(body)),
    )
    con.commit()
    return body


def credit_of(recording: dict[str, Any]) -> str:
    """A candidate's full artist credit, joined the way the tags would write it."""
    names = [a["name"] for a in recording.get("artist-credit") or [] if isinstance(a, dict) and "name" in a]
    return ", ".join(names)


def verdict(recording: dict[str, Any], ours: float | None) -> tuple[str, str]:
    """How much to believe this candidate, and why.

    Three levels, and the middle one is the point: a strong *name* match whose
    duration cannot be checked is not the same evidence as one whose duration
    agrees, and collapsing them is how a live take gets filed as the album cut.
    """
    score = recording.get("score", 0)
    if not isinstance(score, int) or score < STRONG:
        return "uncertain", f"name score {score}"
    length = recording.get("length")
    if ours is None or not isinstance(length, int):
        return "probable", f"name score {score}, no duration to check"
    drift = abs(length / 1000.0 - ours)
    if drift <= DURATION_SLACK:
        return "matched", f"name score {score}, duration within {drift:.1f}s"
    return "uncertain", f"name score {score} but duration off by {drift:.0f}s"


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


def fetch(root: Path, cache: Path, *, limit: int | None) -> None:
    """Look up every file under `root` that does not already name its recording."""
    con = open_cache(cache)
    files = walk(root)
    todo = []
    for path in files:
        tags, _ = read(path)
        if not has_recording_id(tags):
            todo.append((path, tags))
    if limit is not None:
        todo = todo[:limit]

    print(f"{len(files)} files under {root}, {len(todo)} without a recording id")
    if problem := unreadable(todo):
        print(f"refusing to ask MusicBrainz: {problem}")
        return
    asked = cached = 0
    for index, (path, tags) in enumerate(todo, start=1):
        artist = first_artist(tags.get("artist", ""))
        title = searchable_title(tags.get("title", "") or path.stem)
        if not title:
            continue
        before = con.execute("SELECT 1 FROM search WHERE artist = ? AND title = ?", (artist, title)).fetchone()
        search(con, artist, title)
        if before is None:
            asked += 1
        else:
            cached += 1
        if index % 25 == 0:
            print(f"  {index}/{len(todo)}  ({asked} asked, {cached} already cached)", flush=True)
    print(f"done: {asked} asked, {cached} already cached")


def propose(root: Path, cache: Path, out: Path, *, limit: int | None) -> None:
    """Turn the cache into a reviewable file of identifications.

    JSONL rather than a review table because the rows are heterogeneous — a
    confident match carries different evidence from an uncertain one — and
    because this feeds a strict `import`, which wants a machine to read it.
    """
    con = open_cache(cache)
    todo = []
    for path in walk(root):
        tags, seconds = read(path)
        if not has_recording_id(tags):
            todo.append((path, tags, seconds))
    if limit is not None:
        todo = todo[:limit]

    counts = {"matched": 0, "probable": 0, "uncertain": 0, "nothing": 0}
    with out.open("w", encoding="utf-8") as sink:
        sink.write(f"# identified by identify.py against {root}\n")
        for path, tags, seconds in todo:
            artist = first_artist(tags.get("artist", ""))
            title = searchable_title(tags.get("title", "") or path.stem)
            body = search(con, artist, title) if title else None
            candidates = (body or {}).get("recordings") or []
            if not candidates:
                counts["nothing"] += 1
                continue
            best = candidates[0]
            confidence, why = verdict(best, seconds)
            counts[confidence] += 1
            sink.write(
                json.dumps(
                    {
                        "locator": str(path),
                        "mb_recording": best.get("id"),
                        "confidence": confidence,
                        "why": why,
                        "queried": {"artist": artist, "title": title},
                        "theirs": {"title": best.get("title"), "artist": credit_of(best)},
                        "ours": {"title": tags.get("title", ""), "artist": tags.get("artist", "")},
                    }
                )
                + "\n"
            )

    total = sum(counts.values())
    print(f"{total} files without a recording id -> {out}")
    for level in ("matched", "probable", "uncertain", "nothing"):
        share = f"{100 * counts[level] / total:.1f}%" if total else "-"
        print(f"  {level:9s} {counts[level]:5d}  {share}")
    print("\nreview the uncertain ones before anything applies them:")
    print(f"  jq -r 'select(.confidence==\"uncertain\")' {out}")


def main() -> None:
    """Parse the command line and run the requested pass."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="command", required=True)

    fe = sub.add_parser("fetch", help="ask MusicBrainz about files with no recording id")
    fe.add_argument("root")
    fe.add_argument("--limit", type=int, default=None, help="stop after this many files")

    pr = sub.add_parser("propose", help="turn the cache into a reviewable jsonl")
    pr.add_argument("root")
    pr.add_argument("--out", default=None)
    pr.add_argument("--limit", type=int, default=None)

    args = parser.parse_args()
    cache = out_path("identify-cache.sqlite")
    root = Path(args.root).expanduser()
    if not root.exists():
        print(f"no such root: {root}")
        sys.exit(1)

    if args.command == "fetch":
        fetch(root, cache, limit=args.limit)
    else:
        default = out_path(f"identify-{root.name.lower()}.jsonl")
        propose(root, cache, Path(args.out).expanduser() if args.out else default, limit=args.limit)


if __name__ == "__main__":
    main()
