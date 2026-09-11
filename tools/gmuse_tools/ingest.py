#!/usr/bin/env python3
r"""Bring a bought album into the library at the standard the rest of it holds.

A Bandcamp download arrives with almost nothing on it — title, artist, album, a
track number with no total, a year, and a 2 MB cover on every file. The library
around it carries MusicBrainz ids, a release group, sort names, an original
date, a label, disc and track totals, a genre in this project's `Broad; Style`
convention, and ReplayGain. Dropping the new folder in as it comes would leave
one album that answers none of the questions the others do.

## Identification, and the discipline it borrows

The album is matched against MusicBrainz **by name and then verified by
tracklist** — the same rule the Discogs fallback follows in `genre/fetch.py`,
for the same reason. A name search returns plausible wrong answers: `Boavista`
matched a 2-track single of the same name, and `21` matched the standard
edition against a deluxe. So a candidate is fetched, its titles compared, and
**only an `exact` agreement is used**. Anything less is reported and nothing is
written.

## What is deliberately not done here

**Genre and ReplayGain are other passes.** `genre.propose` reads the very
MusicBrainz id this writes, and `rsgain` needs the final file in its final
folder. Doing them here would be a second implementation of each. The order is
ingest, then genre, then rsgain, and `--next` prints it.

    uv run python -m ingest identify ~/Downloads/music/Some\\ Album
    uv run python -m ingest apply ~/Downloads/music/Some\\ Album --artist "Joe Purdy" --write
"""

from __future__ import annotations

import collections
import contextlib
import csv
import difflib
import functools
import json
import re
import shutil
import sqlite3
import sys
import unicodedata
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

import gmuse_core
import typer
from mutagen import File
from mutagen.flac import FLAC
from mutagen.id3 import (
    ID3,
    TALB,
    TDOR,
    TDRC,
    TIT2,
    TMED,
    TPE1,
    TPE2,
    TPOS,
    TPUB,
    TRCK,
    TSO2,
    TSOP,
    TSRC,
    TXXX,
    UFID,
)

from gmuse_tools import bronze
from gmuse_tools.genre.common import compare_key, out_path, tracklist_match, walk, write_tsv

MB: Final = "https://musicbrainz.org/ws/2"
#: Everything needed in one request. Each `inc` is a round trip saved against a
#: service that asks for one request a second.
INC: Final = "recordings+artist-credits+labels+release-groups+isrcs+media"
#: How many name-search candidates are worth fetching a tracklist for.
CANDIDATES: Final = 5

#: How many tracks an `exact` tracklist needs before it is accepted unread.
#:
#: **The rule's strength is combinatorial and vanishes at one.** Twelve titles
#: agreeing at once is hard to do by accident; *one* title agreeing means only
#: that a release exists with that name, which is the weakest possible evidence
#: wearing the strongest verdict. The survey pre-marked three one-track folders
#: — `Becky G & El Alfa/Fulanito`, `Cat Clyde/All The Black`, `Mala/Changes` —
#: as ready to accept unread, and they are exactly the rows a person should
#: look at.
#:
#: Short releases are still *reported* as exact. They are simply not marked.
ENOUGH_TO_TRUST: Final = 4

#: How alike two titles must be before one is taken to name the other.
#:
#: `difflib`'s own default for `get_close_matches`, and `diffs` already pairs
#: with it — so a difference the review file shows as a pair is a difference
#: `seated` will seat as a pair. Two windows onto one judgement.
TITLES_AGREE: Final = 0.6

#: Characters no filesystem in this library's life should have to carry. The
#: library is read on macOS and backed up to a NAS that has seen Windows.
#: Annotated rather than inlined: `dict.fromkeys` infers a `LiteralString` key
#: type that `str.maketrans` will not take, and the comprehension ruff prefers
#: is the form pyrefly rejects. Naming the type satisfies both.
UNSAFE_MAP: Final[dict[str, str]] = dict.fromkeys('/\\:*?"<>|', "_")
UNSAFE: Final = str.maketrans(UNSAFE_MAP)


@dataclass(frozen=True)
class Credit:
    """A MusicBrainz artist credit, in the four shapes the tags want."""

    joined: str
    names: list[str]
    ids: list[str]
    sort: str


@dataclass
class Match:
    """A verified MusicBrainz release."""

    release: dict[str, Any]
    verdict: str
    share: float


#: What a download may hold.
#:
#: This was `.mp3` alone, and a FLAC album therefore had **no front door**:
#: the stage that verifies a tracklist against MusicBrainz skipped every file
#: and reported an empty folder, so a bought FLAC could only be brought in by
#: hand — which is how five albums arrived without a MusicBrainz id between
#: them and minted 67 recordings the library already had.
AUDIO: Final = {".mp3", ".flac"}


def our_titles(folder: Path) -> list[tuple[Path, str]]:
    """Each audio file and the title its tags claim, in track order."""
    out: list[tuple[int, Path, str]] = []
    for path in folder.iterdir():
        if path.suffix.lower() not in AUDIO:
            continue
        if path.suffix.lower() == ".flac":
            flac = FLAC(path)
            title = (flac.get("title") or [path.stem])[0]
            number = (flac.get("tracknumber") or ["0"])[0].split("/")[0]
        else:
            tag = ID3(path)
            title = str(tag["TIT2"].text[0]) if "TIT2" in tag else path.stem
            number = str(tag["TRCK"].text[0]).split("/")[0] if "TRCK" in tag else "0"
        out.append((position(number), path, title))
    return [(p, t) for _, p, t in sorted(out)]


def position(number: str) -> int:
    """The track number a tag claims, or 0 where it does not claim one.

    **A track number is not always a number.** `int()` on the raw tag crashed
    the whole survey on the Aphex Twin corpus, where a vinyl rip carries the
    side in `TRCK` — `One4`, `A1`, `B2` — and one bad file took 44 albums down
    with it. The number is only used to order a tracklist for comparison, so an
    unreadable one costs the ordering of one file and a crash costs everything.

    Leading digits are kept, because `4/12` is already handled by the caller's
    split and `12a` is a numbered take rather than a mystery.
    """
    digits = ""
    for ch in number.strip():
        if not ch.isdigit():
            break
        digits += ch
    return int(digits) if digits else 0


def named_release(mbid: str, ours: list[str]) -> Match | None:
    """The release a person named, taken on their word.

    **The override, and the only thing here that skips the tracklist gate.**
    Some records cannot be matched by title however good the rule: MusicBrainz
    lists every track of *Studio 1 - Variationen* as `[untitled]`, which is the
    sleeve's own labelling — only the numbering carries meaning — so a title
    comparison proves nothing in either direction and never will.

    The verdict is still reported, so the shell history shows what was accepted
    and on whose say-so.
    """
    full = asked(f"{MB}/release/{mbid}", {"inc": INC, "fmt": "json"})
    if not full:
        return None
    titles = [t["title"] for m in full.get("media", []) for t in m.get("tracks", [])]
    _, share = tracklist_match(ours, titles)
    return Match(full, "named", share)


#: The one status that carries an answer.
HTTP_OK: Final = 200

#: The user agent MusicBrainz sees, per its rate-limit policy.
ASKING: Final = {"User-Agent": "gmuse-ingest/0.1 ( https://gitlab.com/gabrielgellner/gmuse )"}


def asked(url: str, params: dict[str, str]) -> dict[str, Any] | None:
    """Ask MusicBrainz, or recall having asked — through `bronze`.

    **This used `genre.fetch.get_json`, which is the raw fetcher underneath a
    cache rather than beside one.** `genre/fetch.py` calls it and then stores
    the *genres* it extracted; `ingest` called it and stored nothing, so every
    survey re-asked MusicBrainz for every release. A 139-album survey is around
    600 requests at one a second, and re-running it to change a column spent
    all of them again.

    `bronze` keys on the url and its parameters and keeps what it was told, so
    a second run over the same library is free. That is the difference between
    a review file you can regenerate and one you dare not.
    """
    status, body = bronze.get(cache(), url, params, headers=ASKING)
    if status != HTTP_OK or not isinstance(body, dict):
        return None
    return body


@functools.cache
def cache() -> sqlite3.Connection:
    """The one cache connection this process uses.

    Opened per call at first, which is a new sqlite handle and a `CREATE TABLE
    IF NOT EXISTS` for every question asked — hundreds of them in a library
    pass, to answer a question the first one already settled.
    """
    return bronze.open_cache()


def missing_tracks(folder: Path) -> list[str]:
    """Where this folder holds fewer tracks than its own tags say the album has.

    **A fragment declares itself.** `Adele/21 (Bonus Tracks - HN826CDE)` holds
    two files numbered `12/13` and `13/13`: the tags say the album has thirteen
    tracks and the folder has two, so this is part of a record that lives
    somewhere else — the other sixteen tracks of *21* are in two sibling
    folders, and no folder-shaped search can ever match the whole.

    Checked from the tags rather than from folder names. A rule about names is
    how `Studio 1 - Variationen` came to be held twice; `12/13` is not a guess.

    Counted **per disc**, because a two-disc album has two tracks numbered 1 and
    each side declares its own total.

    Returns a line per short disc, empty when the folder is whole (or says
    nothing about how whole it should be).
    """
    per_disc: dict[str, tuple[int, set[int]]] = {}
    for path in folder.iterdir():
        if path.suffix.lower() not in AUDIO:
            continue
        tags = File(path, easy=True)
        if tags is None:
            continue
        number = (tags.get("tracknumber") or [""])[0]
        disc = ((tags.get("discnumber") or [""])[0].split("/")[0]) or "1"
        total = 0
        if "/" in number:
            total = int(number.split("/")[1] or 0)
        elif tags.get("tracktotal"):
            total = int((tags["tracktotal"] or ["0"])[0] or 0)
        held, seen = per_disc.get(disc, (0, set()))
        with contextlib.suppress(ValueError):
            seen.add(int(number.split("/")[0]))
        per_disc[disc] = (max(held, total), seen)

    return [
        f"disc {disc}: {len(seen)} of {total} tracks — missing {sorted(set(range(1, total + 1)) - seen)}"
        for disc, (total, seen) in sorted(per_disc.items())
        if total and len(seen) < total
    ]


def whole_album(folder: Path) -> bool:
    """Whether this folder is a record rather than a piece of one, and say so.

    **Refused before the search, not after.** A fragment scores weak against
    the release it belongs to and can score *exact* against a wrong small one:
    a two-track folder will find some two-track single, and that match is
    confident and useless.

    **Unconditional, including under `--release`.** `apply` pairs files against
    the release's tracklist by position, so naming the right release for a
    fragment is worse than naming none: `Adele/21 (Bonus Tracks)` pointed at the
    eighteen-track edition would be tagged as its first two tracks.
    """
    short = missing_tracks(folder)
    if not short:
        return True
    print(f"  {folder.name} is part of an album, not an album:")
    for line in short:
        print(f"    {line}")
    print("  Nothing written. Put the whole record in one folder and identify that.")
    # **`--release` does not get you past this, and must not.** `apply` pairs
    # your files against the release's tracklist *by position*, so a two-file
    # folder pointed at an eighteen-track release would be tagged as tracks one
    # and two of it. The escape hatch is a person saying which release, never a
    # person saying how much of it they have.
    return False


def tagged_album(files: list[Path]) -> str:
    """What the files agree the album is called, or `""` where they do not.

    **The folder is the weaker source, and here it is the wrong one.** Rule 13:
    *ask the authority the question in the terms the authority uses; a query
    that cannot match is not absence.* This corpus files albums as
    `[1989] Red EP [1993]`, so `release:"[1989] Red EP [1993]"` matched nothing
    for all fifty albums — fifty rows of `no candidate` that said only that the
    query was malformed.

    `genre.common.album_key` already keys on tags for the neighbouring reason:
    comparing folder *names* imported one UMFANG record twice, once as `_Ok_`
    and once with the fullwidth quotes the naming rule requires.

    **A majority, not the first file**, because a compilation folder carries
    several album tags and the one that sorts first is not the album. A tie
    returns nothing and the folder name stands, which is the honest answer to
    a folder holding two records.

    Note the inversion this corpus produces: the **path** holds the clean
    artist — `Aphex Twin` — while the **tags** hold `Aphex Twin (aka AFX)`,
    which is a query that cannot match either. Each rung is evidence and
    neither is the answer.
    """
    seen: collections.Counter[str] = collections.Counter()
    for path in files:
        tags = File(path, easy=True)
        if not tags:
            continue
        for name in tags.get("album") or []:
            if name.strip():
                seen[name.strip()] += 1
    ranked = seen.most_common(2)
    if not ranked:
        return ""
    if len(ranked) > 1 and ranked[0][1] == ranked[1][1]:
        return ""
    return ranked[0][0]


#: The library's own moniker convention, written into the artist tag.
#:
#: `Aphex Twin (aka AFX)` on 53 files, `(aka Polygon Window)` on 15, and six
#: more names besides. Twenty years of retagging arrived at the problem #174
#: models and solved it by writing prose into a field.
AKA: Final = re.compile(r"\(\s*aka\s+([^)]+)\)", re.IGNORECASE)


def monikers(files: list[Path]) -> list[str]:
    """Other names the tags credit, for asking a question the authority can answer.

    **A search term and never a fact** — the distinction lesson 18 draws, and
    the reason it matters here is measurable. `Universal Indicator` is the most
    common `(aka)` in this corpus at 59 files, more than AFX, and MusicBrainz
    calls it `member of band` rather than `is person`. Reading these into
    `AliasOf` would be right about 90 files and wrong about 59, and the wrong
    ones fold a two-person project into one person.

    Shaping a *query* with them is safe in a way asserting them is not, because
    the tracklist gate corroborates the answer against something the guess did
    not produce — lesson 1. A wrong moniker finds nothing, or finds a record
    whose titles do not match, and is discarded either way.

    Why it is needed at all: MusicBrainz files these releases under the
    moniker. `artist:"Aphex Twin" AND release:"Analogue Bubblebath EP 1"`
    returns nothing; `artist:"AFX" AND release:"Analogue Bubblebath"` scores
    100. Lesson 13 — the authority uses `AFX`, so the query must.

    A truncated tag is skipped rather than guessed at: 21 files here read
    `Aphex Twin (aka Universal Indi`, cut at 30 characters by an ID3v1 block,
    and the closing bracket is what says the name is whole.
    """
    seen: list[str] = []
    for path in files:
        tags = File(path, easy=True)
        if not tags:
            continue
        for credited in tags.get("artist") or []:
            for found in AKA.findall(credited):
                name = found.strip()
                if name and name not in seen:
                    seen.append(name)
    return seen


def best_match(artist: str, album: str, ours: list[str], files: list[Path]) -> tuple[Match | None, str]:
    """The release this folder holds, and whoever it turned out to be credited to.

    **Asked again as whoever the record was released as** — #178. MusicBrainz
    files `Analogue Bubblebath` under AFX, so
    `artist:"Aphex Twin" AND release:"Analogue Bubblebath EP 1"` returns
    nothing while `artist:"AFX" AND release:"Analogue Bubblebath"` scores 100.
    The first is not absence; it is a question asked as the wrong person.

    Only where the first ask fell short, so an ordinary album spends no extra
    rate limit — and the moniker only ever shapes the query. What comes back
    still has to clear the tracklist gate, which is evidence the guess did not
    produce.
    """
    match = identify(artist, album, ours)
    if match is not None and match.verdict == "exact":
        return match, artist
    credited = artist
    for other in monikers(files):
        alt = identify(other, album, ours)
        if alt is None:
            continue
        if match is None or alt.share > match.share:
            match, credited = alt, other
        if alt.verdict == "exact":
            break
    return match, credited


def identify(artist: str, album: str, ours: list[str]) -> Match | None:
    """Find the release, and refuse anything short of an exact tracklist."""
    found = asked(
        f"{MB}/release",
        {"query": f'artist:"{artist}" AND release:"{album}"', "fmt": "json", "limit": str(CANDIDATES)},
    )
    # **A phrase first, and the loose form only where it found nothing** — the
    # library's album names carry more than the authority's do:
    # `Selected Ambient Works Vol.1 [85-92]` against `Selected Ambient Works
    # 85-92` (an en dash there, in fact), and `Come to Daddy EP` against
    # `Come to Daddy`. As a phrase both
    # return nothing; unquoted both score 100.
    #
    # Loosening the query is safe here and would not be elsewhere: what comes
    # back is still fetched and compared track by track, so a looser search
    # widens the *candidates* and never the acceptances. Anything short of an
    # exact tracklist is reported and nothing is written.
    if not (found or {}).get("releases"):
        found = asked(
            f"{MB}/release",
            {"query": f'artist:"{artist}" AND release:({album})', "fmt": "json", "limit": str(CANDIDATES)},
        )
    best: Match | None = None
    for candidate in (found or {}).get("releases", [])[:CANDIDATES]:
        full = asked(f"{MB}/release/{candidate['id']}", {"inc": INC, "fmt": "json"})
        if not full:
            continue
        titles = [t["title"] for m in full.get("media", []) for t in m.get("tracks", [])]
        verdict, share = tracklist_match(ours, titles)
        if best is None or share > best.share:
            best = Match(full, verdict, share)
        if verdict == "exact":
            return best
    return best


def credit(artist_credit: list[dict[str, Any]]) -> Credit:
    """Read a MusicBrainz artist credit into the shapes the tags want.

    The joined form is what `TPE1` shows — "A feat. B", with the join phrases
    the credit itself carries. The list is what `ARTISTS` gets, which is the
    field that lets one album file under each of its artists (#60) and the one
    thing no string split can reconstruct afterwards.
    """
    return Credit(
        joined="".join(a.get("name", "") + a.get("joinphrase", "") for a in artist_credit),
        names=[a["artist"]["name"] for a in artist_credit],
        ids=[a["artist"]["id"] for a in artist_credit],
        sort="".join(a["artist"].get("sort-name", "") + a.get("joinphrase", "") for a in artist_credit),
    )


def write_tags(path: Path, release: dict[str, Any], disc: dict[str, Any], track: dict[str, Any]) -> None:
    """Put the gold-standard set onto one file, in the form its container uses."""
    if path.suffix.lower() == ".flac":
        write_vorbis(path, release, disc, track)
    else:
        write_id3(path, release, disc, track)


def write_vorbis(path: Path, release: dict[str, Any], disc: dict[str, Any], track: dict[str, Any]) -> None:
    """The same set, under the names a Vorbis comment uses.

    Field names match what this library's existing FLACs already carry — the
    lowercase Picard convention — so a filter written against one format finds
    the other.

    **Multi-value is native here**, which ID3 has to fake: `artists` and
    `musicbrainz_artistid` are real lists, where the ID3 path packs them into a
    `TXXX` frame and a slash-join.
    """
    tag = FLAC(path)
    group = release.get("release-group") or {}
    label_info = (release.get("label-info") or [{}])[0]
    recording = track.get("recording") or {}
    album = credit(release["artist-credit"])
    performer = credit(track["artist-credit"])

    def put(field: str, value: str | list[str] | None) -> None:
        """Set a field, or leave it absent. An empty tag says less than none."""
        if value:
            tag[field] = value if isinstance(value, list) else [value]

    put("title", track["title"])
    put("album", release["title"])
    put("artist", performer.joined)
    put("albumartist", album.joined)
    put("artistsort", performer.sort)
    put("albumartistsort", album.sort)
    put("artists", performer.names)
    put("tracknumber", str(track["position"]))
    put("tracktotal", str(len(disc["tracks"])))
    put("discnumber", str(disc["position"]))
    put("disctotal", str(len(release["media"])))
    put("date", release.get("date"))
    # The *original* release, which is the release group's first — the field
    # #59 needs, and the one that keeps a remaster out of the wrong year.
    if first := group.get("first-release-date"):
        put("originaldate", first)
        put("originalyear", first[:4])
    put("media", disc.get("format"))
    put("label", (label_info.get("label") or {}).get("name"))
    if isrcs := recording.get("isrcs"):
        put("isrc", isrcs[0])
    if script := (release.get("text-representation") or {}).get("script"):
        put("script", script)

    put("musicbrainz_albumid", release["id"])
    put("musicbrainz_albumartistid", album.ids)
    put("musicbrainz_artistid", performer.ids)
    put("musicbrainz_releasetrackid", track["id"])
    put("musicbrainz_releasegroupid", group.get("id"))
    put("musicbrainz_trackid", recording.get("id"))
    if primary := group.get("primary-type"):
        put("releasetype", str(primary).lower())
    if status := release.get("status"):
        put("releasestatus", str(status).lower())
    tag.save()


def write_id3(path: Path, release: dict[str, Any], disc: dict[str, Any], track: dict[str, Any]) -> None:
    """The gold-standard set as ID3v2.4 with `v1=0`.

    `v1=0` so an import cannot reintroduce the ID3v1 block the whole library
    was just cleaned of.
    """
    tag = ID3(path)
    group = release.get("release-group") or {}
    label_info = (release.get("label-info") or [{}])[0]
    recording = track.get("recording") or {}

    album = credit(release["artist-credit"])
    performer = credit(track["artist-credit"])

    tag.setall("TIT2", [TIT2(encoding=3, text=track["title"])])
    tag.setall("TALB", [TALB(encoding=3, text=release["title"])])
    tag.setall("TPE1", [TPE1(encoding=3, text=performer.joined)])
    tag.setall("TPE2", [TPE2(encoding=3, text=album.joined)])
    tag.setall("TSOP", [TSOP(encoding=3, text=performer.sort)])
    tag.setall("TSO2", [TSO2(encoding=3, text=album.sort)])
    tag.setall("TRCK", [TRCK(encoding=3, text=f"{track['position']}/{len(disc['tracks'])}")])
    tag.setall("TPOS", [TPOS(encoding=3, text=f"{disc['position']}/{len(release['media'])}")])

    if date := release.get("date"):
        tag.setall("TDRC", [TDRC(encoding=3, text=date)])
    # The *original* release, which is the release group's first — the field
    # #59 needs, and the one that keeps a remaster out of the wrong year.
    if first := group.get("first-release-date"):
        tag.setall("TDOR", [TDOR(encoding=3, text=first)])
    if fmt := disc.get("format"):
        tag.setall("TMED", [TMED(encoding=3, text=fmt)])
    if label := (label_info.get("label") or {}).get("name"):
        tag.setall("TPUB", [TPUB(encoding=3, text=label)])
    if isrcs := recording.get("isrcs"):
        tag.setall("TSRC", [TSRC(encoding=3, text=isrcs[0])])

    write_identity(tag, release, track, performer, album)
    tag.save(path, v2_version=4, v1=0)


def write_identity(
    tag: ID3,
    release: dict[str, Any],
    track: dict[str, Any],
    performer: Credit,
    album: Credit,
) -> None:
    """The MusicBrainz block: who this is, on which release, in which group."""
    group = release.get("release-group") or {}
    recording = track.get("recording") or {}

    def txxx(desc: str, value: str) -> None:
        tag.delall(f"TXXX:{desc}")
        tag.add(TXXX(encoding=3, desc=desc, text=value))

    # Multi-value, not slash-joined: ID3v2.4 can hold a real list, and this
    # library was just normalised so that it does.
    tag.delall("TXXX:Artists")
    tag.add(TXXX(encoding=3, desc="Artists", text=performer.names))
    txxx("MusicBrainz Album Id", release["id"])
    txxx("MusicBrainz Album Artist Id", "/".join(album.ids))
    txxx("MusicBrainz Artist Id", "/".join(performer.ids))
    txxx("MusicBrainz Release Track Id", track["id"])
    if group.get("id"):
        txxx("MusicBrainz Release Group Id", group["id"])
    if group.get("primary-type"):
        txxx("MusicBrainz Album Type", str(group["primary-type"]).lower())
    if release.get("status"):
        txxx("MusicBrainz Album Status", str(release["status"]).lower())
    if script := (release.get("text-representation") or {}).get("script"):
        txxx("SCRIPT", script)
    if first := group.get("first-release-date"):
        txxx("originalyear", first[:4])
    if recording.get("id"):
        tag.delall("UFID:http://musicbrainz.org")
        tag.add(UFID(owner="http://musicbrainz.org", data=recording["id"].encode()))


def safe_name(number: int, title: str, suffix: str) -> str:
    """`NN Title.ext`, in the shape the rest of the library uses."""
    clean = unicodedata.normalize("NFC", title).translate(UNSAFE)
    clean = re.sub(r"\s+", " ", clean).strip().rstrip(".")
    return f"{number:02d} {clean}{suffix}"


def report(match: Match | None, ours: list[str]) -> bool:
    """Say what was found. Returns whether it is safe to write."""
    if match is None:
        print("  no MusicBrainz release matched that artist and album")
        return False
    release = match.release
    titles = [t["title"] for m in release.get("media", []) for t in m.get("tracks", [])]
    print(f"  {release['title']} — {release.get('date', '?')} — {release.get('country', '??')}")
    print(f"  {release['id']}")
    print(f"  tracklist: {match.verdict} ({match.share:.0%}), {len(ours)} ours vs {len(titles)} theirs")
    # **Named is accepted, and says so.** A person has looked; the count is
    # still shown, because a release with a different number of tracks is
    # almost always the wrong one whatever anybody says.
    if match.verdict == "named":
        print(f"  accepted on your say-so ({len(ours)} ours vs {len(titles)} theirs)")
        if len(ours) != len(titles):
            print("  REFUSED — the track counts differ, which no override should paper over")
            return False
        return True
    if match.verdict != "exact":
        print("\n  NOT exact — nothing written. A name search returns plausible wrong")
        print("  answers, so only an exact tracklist is used unattended.")
        for a, b in zip(ours, titles, strict=False):
            if a != b:
                print(f"    ours: {a!r}  theirs: {b!r}")
        return False
    return True


@dataclass(frozen=True)
class Wanted:
    """Which record this download is, as far as the caller knows.

    `release` is the caller's own answer and skips the tracklist gate; the
    other two are a search, which does not.
    """

    artist: str
    album: str
    release: str


def seated(
    ours: list[tuple[Path, str]], theirs: list[tuple[dict[str, Any], dict[str, Any]]]
) -> list[tuple[tuple[Path, str], tuple[dict[str, Any], dict[str, Any]]]]:
    """Which file becomes which track.

    **By title where a title can say, and by position where it cannot.**

    This was `zip`, and `zip` is wrong in the one case that looks safest.
    `tracklist_match` computes its verdict on **sets**, so `exact` means every
    title exists on both sides and says nothing whatever about the order. Two
    of the thirteen albums accepted from the Aphex Twin review were `exact` at
    100% and had two tracks transposed against MusicBrainz's running order:

        Hangable Auto Bulb   pos 7  'Every Day'    would have been tagged 'Arched Maid Via RDJ'
        Dokey Rhubarb EP     pos 2  'Icct Hedral'  would have been tagged 'Vaz Deferenz'

    Silent, and worst on the strongest verdict: a high share is exactly when
    every title is present on both sides, which is exactly when a reordering
    leaves no trace in the diff. And `exact` is the verdict `survey` pre-marks
    for acceptance unread, so the unattended path carried the whole risk.

    **Best pair first, not first-come.** Assigning greedily in our order would
    let `We Have Arrived (Aphex Twin QQT mix)` claim the `TTQ` track it is
    nearly as close to; sorting every candidate pair by similarity and taking
    them strongest-first lets each exact title win before its near-twin gets a
    chance.

    **Whatever is left falls back to position**, which is the honest answer
    where titles cannot decide — a release MusicBrainz lists as `[untitled]`
    throughout, or this corpus's own `(Untitled) Red A1`. That is the case
    `named_release` exists for, and there position is the only signal there is.
    """
    keyed_ours = [compare_key(t) for _, t in ours]
    keyed_theirs = [compare_key(t["title"]) for _, t in theirs]
    scored = [
        (difflib.SequenceMatcher(None, a, b).ratio(), i, j)
        for i, a in enumerate(keyed_ours)
        for j, b in enumerate(keyed_theirs)
    ]

    def strongest(scored_pair: tuple[float, int, int]) -> tuple[float, int, int]:
        ratio, i, j = scored_pair
        return (-ratio, i, j)

    scored.sort(key=strongest)

    taken_ours: set[int] = set()
    taken_theirs: set[int] = set()
    seats: dict[int, int] = {}
    for ratio, i, j in scored:
        if ratio < TITLES_AGREE:
            break
        if i in taken_ours or j in taken_theirs:
            continue
        seats[i] = j
        taken_ours.add(i)
        taken_theirs.add(j)

    spare = [j for j in range(len(theirs)) if j not in taken_theirs]
    for i in range(len(ours)):
        if i not in seats and spare:
            seats[i] = spare.pop(0)
    return [(ours[i], theirs[j]) for i, j in sorted(seats.items())]


def apply(folder: Path, wanted: Wanted, dest: Path | None, *, write: bool) -> None:
    """Identify, tag, rename, and optionally move into the library."""
    pairs = our_titles(folder)
    if not pairs:
        print(f"  no audio in {folder}")
        return
    if not whole_album(folder):
        return
    ours = [t for _, t in pairs]
    match = (
        named_release(wanted.release, ours)
        if wanted.release
        else identify(wanted.artist, wanted.album or folder.name, ours)
    )
    if not report(match, ours) or match is None:
        return
    if not write:
        print("\n  dry run — pass --write to tag and move")
        return

    release = match.release
    tally: collections.Counter[str] = collections.Counter()
    flat = [(m, t) for m in release["media"] for t in m["tracks"]]
    for (path, _), (disc, track) in seated(pairs, flat):
        write_tags(path, release, disc, track)
        renamed = path.with_name(safe_name(track["position"], track["title"], path.suffix))
        if renamed != path:
            path.rename(renamed)
        tally["tagged"] += 1

    if dest is not None:
        dest.mkdir(parents=True, exist_ok=True)
        for path in sorted(folder.iterdir()):
            shutil.move(str(path), str(dest / path.name))
        folder.rmdir()
        tally["moved into the library"] += 1
        print(f"  -> {dest}")

    for key, n in sorted(tally.items()):
        print(f"  {n:4d}  {key}")
    print("\n  next: `genre.propose` then `rsgain easy` on the album folder,")
    print("  and `tags survey` to confirm nothing reintroduced an ID3v1 block.")


#: Accept a release the caller has checked, skipping the tracklist gate.
#:
#: For records no title comparison can settle — MusicBrainz lists every track
#: of `Studio 1 - Variationen` as `[untitled]`, which is the sleeve's own
#: labelling, so only the numbering carries meaning.
LIB = typer.Argument(Path("~/MusicLibrary"), help="library root")
RELEASE = typer.Option("", help="a MusicBrainz release id you have checked yourself")


def diffs(ours: list[str], theirs: list[str]) -> list[dict[str, str | None]]:
    """What disagrees, **as a pair**: what we call it and what they call it.

    A share says how much disagrees and never what, and a list of our own
    titles says what we have and never what it would become — which is the
    question a reviewer is actually asking. `ours 'I Found a Bot'` is a
    complaint; `'I Found a Bot' -> 'I Found a Boy'` is a decision.

    **Paired by similarity, not by position**, because the verdict is computed
    on sets and a release whose tracks are ordered differently would otherwise
    report every line as a difference. `difflib` is enough here and is already
    in the standard library.

    A title with nothing close on the other side is paired with `None`, because
    that is the interesting case: it means the two records really differ rather
    than one of them being spelled oddly.

    **Every one of them, and structured.** This joined the first three with
    ` · ` and finished `+9 more`, which hid the answer to the only question the
    row exists to ask. Accepting a release means accepting *all* of its titles,
    so a reviewer who cannot see all of them is being asked to take the rest on
    trust — and the rest is where a wrong edition hides.

    Structured rather than a longer line for two reasons. A reader that
    understands JSON renders twenty pairs as twenty rows; twenty pairs joined
    by ` · ` are one line nobody reads to the end. And `json.dumps` escapes a
    tab, where a raw title carrying one makes `write_tsv` refuse the row — so
    the column that most wants free text becomes the one that cannot break the
    file.
    """
    theirs_keys = {compare_key(t) for t in theirs}
    ours_keys = {compare_key(t) for t in ours}
    only_ours = [t for t in ours if compare_key(t) not in theirs_keys]
    only_theirs = [t for t in theirs if compare_key(t) not in ours_keys]

    shown: list[dict[str, str | None]] = []
    left = list(only_theirs)
    for mine in only_ours:
        near = difflib.get_close_matches(compare_key(mine), [compare_key(t) for t in left], n=1, cutoff=0.6)
        if near:
            i = next(i for i, t in enumerate(left) if compare_key(t) == near[0])
            shown.append({"ours": mine, "theirs": left.pop(i)})
        else:
            shown.append({"ours": mine, "theirs": None})
    shown += [{"ours": None, "theirs": t} for t in left]
    return shown


def as_cell(found: list[dict[str, object]] | list[dict[str, str | None]]) -> str:
    r"""One column of structured detail, safe to put in a tab-separated file.

    `ensure_ascii=False` because the titles are the point and an escaped `\u00e9`
    is not a title anybody can read. The tab and the newline are still escaped,
    which is the whole reason this is safe here.
    """
    return json.dumps(found, ensure_ascii=False) if found else ""


#: How far two durations may sit apart and still be the same track, in seconds.
#:
#: Generous on purpose. A tagger computes `size / first frame's bitrate` where
#: a file carries no Xing header — measured on this library, 34 files wrong by
#: more than two seconds and one claiming 1735s for a 287s track. This is a
#: second opinion offered to a person, so a false *disagreement* costs more
#: than a loose match.
DURATION_TOL: Final = 3.0


def our_seconds(files: list[Path]) -> list[float | None]:
    """How long each file plays, as its tags compute it.

    **Not what `shelf import` measures.** `audio_id` counts packets and is
    exact; this runs before import, where that number does not exist yet.
    """
    out: list[float | None] = []
    for p in sorted(files):
        try:
            f = File(p)
            out.append(f.info.length if f is not None and f.info else None)
        except Exception:  # noqa: BLE001 - a duration is a nicety, never a gate
            out.append(None)
    return out


def duration_share(ours: list[float | None], release: dict[str, Any]) -> float | None:
    """How much of the tracklist agrees by *length* rather than by name.

    **A second opinion, and deliberately not a gate** — #178 measured it. Over
    the 34 Aphex discs the title gate could not confirm, duration agreed more
    on 6, less on 6, and about the same on 22. That is noise, not a signal, and
    a rule reading it would have accepted two releases and rejected six the
    titles had right.

    It earns its column on the two it settles outright. `A Live Collection` and
    `Sonar Live` share **no** title with their release and match every length:
    our titles there are local placeholders — `(Untitled) Peel Session 01` —
    so the title gate is not reporting disagreement, it is reporting that it
    has nothing to compare. Duration is the only evidence in the row.
    """
    theirs = [t.get("length") for m in release.get("media", []) for t in m.get("tracks", [])]
    theirs = [ms / 1000 for ms in theirs if ms]
    if not theirs or not ours:
        return None
    # **`strict=False` on purpose**: the two lists differ in length whenever
    # we hold part of a record, which is the ordinary case here. The extra
    # tracks are counted by the denominator below rather than raised — holding
    # 6 of 8 is 75% agreement at best, never 100%.
    hit = sum(1 for a, b in zip(ours, theirs, strict=False) if a and abs(a - b) <= DURATION_TOL)
    return hit / max(len(ours), len(theirs))


#: What a title looks like when it names nothing — a local placeholder.
#:
#: `(Untitled) Analogue Bubblebath 05`, `(Untitled) Red A1`. A share of 0%
#: against these is **not evidence against the release**: there was nothing to
#: compare. Flagged so a person reading the row knows which kind of nothing it
#: is, which is the same distinction `Held` draws for a part-held release.
PLACEHOLDER: Final = re.compile(r"^\s*\(untitled\)|^\s*(?:track|untitled)\s*\d*\s*$", re.IGNORECASE)


def placeholders(titles: list[str]) -> int:
    """How many of our titles name nothing."""
    return sum(1 for t in titles if PLACEHOLDER.match(t or ""))


def survey(root: Path, out: Path) -> None:
    """Identify every album that carries no MusicBrainz id. Writes no tags.

    **The review file is the point.** `apply` accepts only an exact tracklist,
    which is right unattended and leaves the interesting half untouched: an
    album whose tags hold a typo, or which is one of several editions. Those
    are a minute's reading each, and unreadable as 139 separate command runs.
    """
    rows: list[dict[str, object]] = []
    albums = sorted({p.parent for p in walk(root)})
    for i, d in enumerate(albums, 1):
        files = [p for p in d.iterdir() if p.suffix.lower() in AUDIO]
        if not files:
            continue
        if any((File(p, easy=True) or {}).get("musicbrainz_trackid") for p in files):
            continue
        rel = str(d.relative_to(root))
        artist, _, album = rel.partition("/")
        album = tagged_album(files) or album
        ours = [t for _, t in our_titles(d)]
        print(f"  {i}/{len(albums)} {rel}", file=sys.stderr)
        if short := missing_tracks(d):
            rows.append(
                {
                    "accept": "",
                    "verdict": "part of an album",
                    "share": "",
                    "dur": "",
                    "blank": "",
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "credited": "",
                    "differs": as_cell([{"short": line} for line in short]),
                }
            )
            continue
        try:
            match, credited = best_match(artist, album, ours, files)
        except Exception as e:  # noqa: BLE001 - one album must not end the survey
            # **A long pass has to survive one bad album.** Losing the run
            # means losing every rate limit spent getting to it, and the file
            # this writes is the only thing that made the work reviewable.
            print(f"      failed: {e}", file=sys.stderr)
            rows.append(
                {
                    "accept": "",
                    "verdict": "error",
                    "share": "",
                    "dur": "",
                    "blank": "",
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "credited": "",
                    "differs": as_cell([{"error": str(e)}]),
                }
            )
            continue
        if match is None:
            rows.append(
                {
                    "accept": "",
                    "verdict": "no candidate",
                    "share": "",
                    "dur": "",
                    "blank": "",
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "credited": "",
                    "differs": "",
                }
            )
            continue
        theirs = [t["title"] for m in match.release.get("media", []) for t in m.get("tracks", [])]
        # **Say who it matched as, where that is not the folder** — its own
        # column, because a person reading this row is being asked to accept a
        # release credited to somebody the folder does not name, and that is a
        # fact about the row rather than a note about its differences.
        # `accept` reads this file with `DictReader`, so a column costs it
        # nothing.
        # **Two more columns of evidence, and neither is a gate.** `accept`
        # still turns on the tracklist alone; these say what *else* is true of
        # a row somebody is being asked to read. `blank` is the one that
        # changes how a 0% reads — no agreement against placeholder titles is
        # not disagreement, it is nothing to compare.
        dur = duration_share(our_seconds(files), match.release)
        blank = placeholders(ours)
        rows.append(
            {
                "accept": "y" if match.verdict == "exact" else "",
                "verdict": match.verdict,
                "share": f"{match.share:.0%}",
                "dur": "" if dur is None else f"{dur:.0%}",
                "blank": blank or "",
                "ours": len(ours),
                "theirs": len(theirs),
                "album": rel,
                "release": match.release["id"],
                "credited": "" if credited == artist else credited,
                "differs": as_cell(diffs(ours, theirs)),
            }
        )
    # **Ordered by what wants reading**, not alphabetically. A file of 139 rows
    # is only reviewable if the ten that need a decision are at the top.
    rank = {"exact": 0, "close": 1, "weak": 2, "part of an album": 3, "no candidate": 4, "error": 5}

    def reading_order(row: dict[str, object]) -> tuple[int, int, str]:
        return (rank.get(str(row["verdict"]), 9), -int(str(row["ours"] or 0)), str(row["album"]))

    rows.sort(key=reading_order)
    write_tsv(out, rows)
    tally = collections.Counter(str(r["verdict"]) for r in rows)
    for k, n in tally.most_common():
        print(f"{n:5}  {k}", file=sys.stderr)
    print(f"\n{len(rows)} album(s) -> {out}", file=sys.stderr)
    print("`exact` rows are marked `y` already. Read the rest and mark what you accept.", file=sys.stderr)


def held_discs(folder: Path) -> tuple[str, set[int]]:
    """The MusicBrainz release this folder's files claim, and which discs of it.

    Read off the **tags**, because that is what `import` read to mint the
    release: a slot proposed against a release id the library never saw is
    refused, and refused loudly is the right outcome — the ids only agree where
    the files carry `musicbrainz_albumid`.

    The disc numbers come from the files too, and not from the release, because
    a two-disc record often arrives as two folders and the library holds the
    media it was shown. Proposing a tracklist for a disc nothing here holds
    would be inventing the disc.
    """
    ids: set[str] = set()
    discs: set[int] = set()
    for path in folder.iterdir():
        if path.suffix.lower() not in AUDIO:
            continue
        tags = File(path, easy=True) or {}
        for mbid in tags.get("musicbrainz_albumid") or []:
            ids.add(str(mbid))
        raw = (tags.get("discnumber") or ["1"])[0]
        with contextlib.suppress(ValueError):
            discs.add(int(str(raw).split("/")[0] or 1))
    # **One release or none.** A folder whose files disagree about which
    # pressing they are from is a folder two records got mixed in, and guessing
    # which is right would catalogue the wrong tracklist over both.
    return (next(iter(ids)) if len(ids) == 1 else ""), (discs or {1})


def slots_of(disc: dict[str, Any]) -> list[dict[str, Any]]:
    """One medium's tracks, in the shape a `tracklisted` proposal wants.

    **The recording id, not the track id.** MusicBrainz has both and they name
    different things: the track is this release's entry, the recording is the
    performance — and the recording is what a copy bought later, off any
    release, would also be. Sending the track id would mint a slot no file
    could ever converge with.

    **The artists go as a list, never the joined line.** A joined string mints
    one artist called `Armand Van Helden,Dizzee Rascal` — the bug #152 exists
    to repair — and asserted on the MusicBrainz rung it would outrank the tag
    it came from. Sent as a list it produces exactly the facts #152's own
    proposal produces, so the two converge instead of rendering the credit
    twice.
    """
    out = []
    for track in disc.get("tracks", []):
        recording = track.get("recording") or {}
        artists = credit(track.get("artist-credit") or []).names
        if not recording.get("id") or not artists:
            continue
        # **The recording's title, not the track's**, because that is the
        # field the Rust side catalogues the *recording* with. Sending the
        # track's would have catalogued MusicBrainz recording `7c38270d` — which
        # is called `Phobos` — as `Phobos (Synthapella)`, the name *Luna* prints
        # for it. The sleeve's name goes below, where it belongs.
        slot = {
            "position": track["position"],
            "recording": recording["id"],
            "title": recording.get("title") or track.get("title") or "",
            "artists": artists,
        }
        # **What the sleeve prints, where the release really renames it.**
        #
        # MusicBrainz carries both titles and this used to collapse them, so
        # the Rust side sent `title_on_release: None` on every row with the
        # note that "a tracklist read straight off the release cannot tell the
        # difference". It can — it just was not looking.
        #
        # **Through `compare_key`, so punctuation is not a rename.** Measured
        # on *Luna*: 3 of 34 slots differ and two of them are typography — a
        # non-breaking hyphen in `Phobos (Pan-Pot remix)` and a curly
        # apostrophe in `Triton (Gaiser's Trial Tone remix)`. The third is
        # real: the recording is `Phobos` and the release prints
        # `Phobos (Synthapella)`.
        printed, called = track.get("title") or "", recording.get("title") or ""
        if printed and called and compare_key(printed) != compare_key(called):
            slot["title_on_release"] = printed
        out.append(slot)
    return out


def releases_held(lib: gmuse_core.Library) -> dict[str, str]:
    """Which release the library says each folder's copies sit on — #203.

    By folder, because that is the unit a tracklist is about and the unit the
    tags are read from. A folder holding copies from two releases has no single
    answer and gets whichever came last; that is a folder [`missing_tracks`]
    already treats as suspect.
    """
    held: dict[str, str] = {}
    for row in lib.query(every_copy=True):
        locator, release = row.get("locator"), row.get("release")
        if locator and release:
            held[str(Path(locator).parent)] = release
    return held


def agrees(lib: gmuse_core.Library, held: dict[str, str], folder: Path, mbid: str) -> bool:
    """Whether the tag's release is the one the library holds these copies on.

    **The tag is a candidate, not an identity** — #203, and #199's shape. A
    `tracklisted` fact names a release; taking that release from whatever a
    tagger last wrote means a stale tag writes a *permanent* fact against
    somebody else's pressing, in a log that only forgets by being superseded.

    So the id the tag claims is derived and compared to what the library says.
    A disagreement is a folder to skip, not a fact to write.

    **True for a folder the library holds nothing from**, which is the ordinary
    case for a download being prepared: there is nothing to disagree with yet,
    and refusing it would stop this pass working on exactly what it is for.
    """
    says = held.get(str(folder))
    return says is None or lib.release_from_musicbrainz(mbid) == says


def tracklist(root: Path, out: Path) -> None:
    """Write a proposal cataloguing the whole tracklist of every identified album.

    The second half of #155. Identification already takes a release's id and
    then throws the tracklist away, so the library can say it holds fifteen
    files and cannot name the three it is missing — `gmuse library apply` on
    what this writes is what fills that in.

    **Every album with an id, not only the short ones.** A tracklist that
    matches what is held costs nothing (`apply` drops facts the log already
    says) and is what sets the denominator #134 asked for; restricting it to
    folders that look incomplete would mean trusting the count we are trying to
    establish.
    """
    lib = gmuse_core.Library.open()
    held = releases_held(lib)
    folders = sorted({f.parent for f in walk(root)})
    lines: list[str] = []
    tally: collections.Counter[str] = collections.Counter()
    # **Written as it goes, not at the end.** MusicBrainz answers a heavy `inc`
    # in anything from 0.4 to 25 seconds, so this pass is an hour on a library
    # this size and the temptation to interrupt it is constant. Holding the
    # whole file until the last folder means an interrupted run produces
    # nothing — and the responses are cached, so the *next* run is cheap only
    # for the part that already succeeded. Both halves should survive.
    handle = out.open("w")
    for folder in folders:
        mbid, discs = held_discs(folder)
        if not mbid:
            tally["no release id in the tags"] += 1
            continue

        if not agrees(lib, held, folder, mbid):
            tally["the tag names a different release than the library holds"] += 1
            continue
        release = asked(f"{MB}/release/{mbid}", {"inc": INC, "fmt": "json"})
        if not release:
            tally["release not found"] += 1
            continue
        media = {m["position"]: m for m in release.get("media", [])}
        for disc in sorted(discs):
            found = media.get(disc)
            if found is None:
                tally["disc not on that release"] += 1
                continue
            tracks = slots_of(found)
            if not tracks:
                tally["no tracklist"] += 1
                continue
            line = json.dumps(
                {
                    "fact": "tracklisted",
                    "release": mbid,
                    "position": disc,
                    "format": found.get("format"),
                    "tracks": tracks,
                }
            )
            # **Checked before it is written, not after** — #198. The whole
            # file cannot be validated at the end because it is written as it
            # goes, for the reason above: an interrupted hour should keep what
            # it learned. `check` is `propose` without the writing.
            lib.check([line])
            handle.write(line + "\n")
            handle.flush()
            lines.append(line)
            tally["discs catalogued"] += 1
            tally["slots"] += len(tracks)

    handle.close()
    for key, n in sorted(tally.items()):
        print(f"{n:6d}  {key}", file=sys.stderr)
    print(f"\n{len(lines)} proposal(s) -> {out}", file=sys.stderr)
    print("Apply with `gmuse library apply --write`.", file=sys.stderr)
    print(
        "A disc the library has not seen is reported `unknown` and written off — "
        "that means the files carry no album id and want `ingest accept` first.",
        file=sys.stderr,
    )


def accept(root: Path, review: Path, *, write: bool) -> None:
    """Bring in every album the review marks `y`, on the release it names."""
    rows = [r for r in csv.DictReader(review.open(), delimiter="\t") if r["accept"].strip().lower() == "y"]
    print(f"{len(rows)} album(s) marked")
    for r in rows:
        d = root / r["album"]
        if not d.is_dir():
            print(f"  gone: {r['album']}")
            continue
        print(f"\n== {r['album']}")
        artist, _, album = r["album"].partition("/")
        apply(d, Wanted(artist=artist, album=album, release=r["release"]), None, write=write)


def register(app: typer.Typer) -> None:
    """Attach the importer."""
    ingest_app = typer.Typer(no_args_is_help=True, help=__doc__)
    app.add_typer(ingest_app, name="ingest")

    # **Typer reads this parameter list as the command's interface**, so the
    # options cannot be grouped into an object the way `apply` groups them —
    # a struct here would be one positional argument on the command line.
    @ingest_app.command("survey")
    def survey_cmd(root: Path = LIB) -> None:
        """Identify every album carrying no MusicBrainz id. Writes no tags."""
        survey(root.expanduser(), out_path("ingest-review.tsv"))

    @ingest_app.command("tracklist")
    def tracklist_cmd(root: Path = LIB) -> None:
        """Catalogue the whole tracklist of every identified album — #155."""
        tracklist(root.expanduser(), out_path("tracklists.jsonl"))

    @ingest_app.command("accept")
    def accept_cmd(
        root: Path = LIB, *, write: bool = typer.Option(default=False, help="actually move and tag")
    ) -> None:
        """Bring in every album the review marks `y`, on the release it names."""
        accept(root.expanduser(), out_path("ingest-review.tsv"), write=write)

    @ingest_app.command("apply")
    def apply_cmd(  # noqa: PLR0913
        folder: Path = typer.Argument(..., help="the download to bring in"),
        artist: str = typer.Option(..., help="who it is by"),
        album: str = typer.Option("", help="defaults to the folder name"),
        release: str = RELEASE,
        dest: Path | None = typer.Option(None, help="album folder in the library to move into"),
        *,
        write: bool = typer.Option(default=False, help="actually move and tag"),
    ) -> None:
        """Identify a download against MusicBrainz and bring it in."""
        apply(
            folder.expanduser(),
            Wanted(artist=artist, album=album, release=release),
            dest.expanduser() if dest else None,
            write=write,
        )
