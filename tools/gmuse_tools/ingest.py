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
import re
import shutil
import sys
import unicodedata
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

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

from gmuse_tools.genre.common import compare_key, out_path, tracklist_match, walk, write_tsv
from gmuse_tools.genre.fetch import get_json

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
        out.append((int(number or 0), path, title))
    return [(p, t) for _, p, t in sorted(out)]


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
    full = get_json(f"{MB}/release/{mbid}", {"inc": INC, "fmt": "json"})
    if not full:
        return None
    titles = [t["title"] for m in full.get("media", []) for t in m.get("tracks", [])]
    _, share = tracklist_match(ours, titles)
    return Match(full, "named", share)


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


def identify(artist: str, album: str, ours: list[str]) -> Match | None:
    """Find the release, and refuse anything short of an exact tracklist."""
    query = f'artist:"{artist}" AND release:"{album}"'
    found = get_json(f"{MB}/release?query={urllib.parse.quote(query)}&fmt=json&limit={CANDIDATES}", {})
    best: Match | None = None
    for candidate in (found or {}).get("releases", [])[:CANDIDATES]:
        full = get_json(f"{MB}/release/{candidate['id']}", {"inc": INC, "fmt": "json"})
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
    for (path, _), (disc, track) in zip(pairs, flat, strict=False):
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


def diffs(ours: list[str], theirs: list[str], limit: int = 3) -> str:
    """What disagrees, **as a pair**: what we call it and what they call it.

    A share says how much disagrees and never what, and a list of our own
    titles says what we have and never what it would become — which is the
    question a reviewer is actually asking. `ours 'I Found a Bot'` is a
    complaint; `'I Found a Bot' -> 'I Found a Boy'` is a decision.

    **Paired by similarity, not by position**, because the verdict is computed
    on sets and a release whose tracks are ordered differently would otherwise
    report every line as a difference. `difflib` is enough here and is already
    in the standard library.

    A title with nothing close on the other side is shown alone, because that
    is the interesting case: it means the two records really differ rather than
    one of them being spelled oddly.
    """
    theirs_keys = {compare_key(t) for t in theirs}
    ours_keys = {compare_key(t) for t in ours}
    only_ours = [t for t in ours if compare_key(t) not in theirs_keys]
    only_theirs = [t for t in theirs if compare_key(t) not in ours_keys]

    shown: list[str] = []
    left = list(only_theirs)
    for mine in only_ours:
        near = difflib.get_close_matches(compare_key(mine), [compare_key(t) for t in left], n=1, cutoff=0.6)
        if near:
            i = next(i for i, t in enumerate(left) if compare_key(t) == near[0])
            shown.append(f"{mine!r} -> {left.pop(i)!r}")
        else:
            shown.append(f"{mine!r} -> (nothing like it)")
    shown += [f"(nothing like it) -> {t!r}" for t in left]

    head = " · ".join(shown[:limit])
    return head + (f" · +{len(shown) - limit} more" if len(shown) > limit else "")


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
        ours = [t for _, t in our_titles(d)]
        print(f"  {i}/{len(albums)} {rel}", file=sys.stderr)
        if short := missing_tracks(d):
            rows.append(
                {
                    "accept": "",
                    "verdict": "part of an album",
                    "share": "",
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "differs": "; ".join(short),
                }
            )
            continue
        try:
            match = identify(artist, album, ours)
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
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "differs": str(e)[:80],
                }
            )
            continue
        if match is None:
            rows.append(
                {
                    "accept": "",
                    "verdict": "no candidate",
                    "share": "",
                    "ours": len(ours),
                    "theirs": "",
                    "album": rel,
                    "release": "",
                    "differs": "",
                }
            )
            continue
        theirs = [t["title"] for m in match.release.get("media", []) for t in m.get("tracks", [])]
        rows.append(
            {
                "accept": "y" if match.verdict == "exact" else "",
                "verdict": match.verdict,
                "share": f"{match.share:.0%}",
                "ours": len(ours),
                "theirs": len(theirs),
                "album": rel,
                "release": match.release["id"],
                "differs": diffs(ours, theirs),
            }
        )
    # **Ordered by what wants reading**, not alphabetically. A file of 139 rows
    # is only reviewable if the ten that need a decision are at the top.
    rank = {"exact": 0, "close": 1, "weak": 2, "part of an album": 3, "no candidate": 4, "error": 5}

    def reading_order(row: dict[str, object]) -> tuple[int, int, str]:
        return (rank.get(str(row["verdict"]), 9), -int(row["ours"] or 0), str(row["album"]))

    rows.sort(key=reading_order)
    write_tsv(out, rows)
    tally = collections.Counter(str(r["verdict"]) for r in rows)
    for k, n in tally.most_common():
        print(f"{n:5}  {k}", file=sys.stderr)
    print(f"\n{len(rows)} album(s) -> {out}", file=sys.stderr)
    print("`exact` rows are marked `y` already. Read the rest and mark what you accept.", file=sys.stderr)


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
