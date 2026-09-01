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

import argparse
import collections
import re
import shutil
import unicodedata
import urllib.parse
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Final

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

from genre.common import tracklist_match
from genre.fetch import get_json

MB: Final = "https://musicbrainz.org/ws/2"
#: Everything needed in one request. Each `inc` is a round trip saved against a
#: service that asks for one request a second.
INC: Final = "recordings+artist-credits+labels+release-groups+isrcs+media"
#: How many name-search candidates are worth fetching a tracklist for.
CANDIDATES: Final = 5

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


def our_titles(folder: Path) -> list[tuple[Path, str]]:
    """Each mp3 and the title its tags claim, in track order."""
    out: list[tuple[int, Path, str]] = []
    for path in folder.iterdir():
        if path.suffix.lower() != ".mp3":
            continue
        tag = ID3(path)
        title = str(tag["TIT2"].text[0]) if "TIT2" in tag else path.stem
        number = str(tag["TRCK"].text[0]).split("/")[0] if "TRCK" in tag else "0"
        out.append((int(number or 0), path, title))
    return [(p, t) for _, p, t in sorted(out)]


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
    """Put the gold-standard set onto one file.

    Written as ID3v2.4 with `v1=0`, so an import cannot reintroduce the ID3v1
    block the whole library was just cleaned of.
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
    if match.verdict != "exact":
        print("\n  NOT exact — nothing written. A name search returns plausible wrong")
        print("  answers, so only an exact tracklist is used unattended.")
        for a, b in zip(ours, titles, strict=False):
            if a != b:
                print(f"    ours: {a!r}  theirs: {b!r}")
        return False
    return True


def apply(folder: Path, artist: str, album: str, dest: Path | None, *, write: bool) -> None:
    """Identify, tag, rename, and optionally move into the library."""
    pairs = our_titles(folder)
    if not pairs:
        print(f"  no mp3s in {folder}")
        return
    ours = [t for _, t in pairs]
    match = identify(artist, album or folder.name, ours)
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


def main() -> None:
    """Parse the command line and run the requested phase."""
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest="phase", required=True)
    for name in ("identify", "apply"):
        s = sub.add_parser(name)
        s.add_argument("folder")
        s.add_argument("--artist", required=True)
        s.add_argument("--album", default="", help="defaults to the folder name")
        if name == "apply":
            s.add_argument("--dest", help="album folder in the library to move into")
            s.add_argument("--write", action="store_true")
    args = parser.parse_args()

    folder = Path(args.folder).expanduser()
    apply(
        folder,
        args.artist,
        args.album,
        Path(args.dest).expanduser() if getattr(args, "dest", None) else None,
        write=getattr(args, "write", False),
    )


if __name__ == "__main__":
    main()
