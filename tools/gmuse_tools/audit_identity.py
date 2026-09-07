#!/usr/bin/env python3
"""Which albums no authority has named — the gap that duplicates a record.

An album with no MusicBrainz id is not merely thinner than its neighbours. Its
recordings are minted from their **audio bytes**, so a second copy of the same
music — a FLAC bought to replace an MP3, a re-download, a friend's rip — hashes
differently by design and becomes a *second recording* holding its own ratings
and plays.

That is not hypothetical. Five FLAC albums brought in against MP3s already held
produced 67 new recordings for music the library already had, because neither
side carried an id to converge on. Retagging both sides from one MusicBrainz
release turned 30 of them into what they are — one recording, two encodings.

So this reports what `ingest apply` should be pointed at, worst first. It reads
tags and changes nothing.

    uv run gmuse-tools audit identity ~/MusicLibrary
    uv run gmuse-tools audit identity ~/MusicLibrary --out out/unidentified.tsv
"""

from __future__ import annotations

import collections
from pathlib import Path
from typing import Final

import typer
from mutagen import File

from gmuse_tools.genre.common import walk, write_tsv

#: How many rows to print before deferring to the file. Long enough to act on,
#: short enough to read.
SHOWN: Final = 20


def coverage(root: Path) -> dict[str, tuple[int, int]]:
    """Per album folder: how many files it holds, and how many are identified.

    Keyed by folder rather than by the `album` tag, because the question is
    "what should `ingest` be run on", and `ingest` takes a folder.
    """
    out: dict[str, list[int]] = collections.defaultdict(lambda: [0, 0])
    for path in walk(root):
        try:
            tags = File(path, easy=True)
        except Exception as e:  # noqa: BLE001 - an unreadable file is another audit's business
            print(f"  unreadable, skipped: {path.name} — {e}")
            continue
        if tags is None:
            continue
        key = str(path.parent.relative_to(root))
        out[key][0] += 1
        if tags.get("musicbrainz_trackid"):
            out[key][1] += 1
    return {k: (v[0], v[1]) for k, v in out.items()}


def report(found: dict[str, tuple[int, int]], out: Path | None) -> None:
    """Say what is unidentified, worst first."""
    whole = {k: v for k, v in found.items() if v[1] == v[0]}
    part = {k: v for k, v in found.items() if 0 < v[1] < v[0]}
    none = {k: v for k, v in found.items() if v[1] == 0}
    files = sum(v[0] for v in found.values())
    print(f"{len(found)} album folders, {files} files")
    print(f"   identified throughout : {len(whole):4} albums, {sum(v[0] for v in whole.values()):5} files")
    print(f"   partly identified     : {len(part):4} albums, {sum(v[0] for v in part.values()):5} files")
    print(f"   no MusicBrainz id     : {len(none):4} albums, {sum(v[0] for v in none.values()):5} files")

    def worst_first(row: tuple[str, tuple[int, int]]) -> tuple[int, str]:
        return (-row[1][0], row[0])

    def least_covered(row: tuple[str, tuple[int, int]]) -> tuple[float, int]:
        _, (n, have) = row
        return (have / n, -n)

    # **Partly is worse than none**, and listed first for that reason: a folder
    # where some tracks resolve and some do not will converge in half and split
    # in half, which is harder to notice than an album that is uniformly absent.
    for label, group in (("partly identified", part), ("no MusicBrainz id", none)):
        if not group:
            continue
        print(f"\n  {label}:")
        for name, (n, have) in sorted(group.items(), key=worst_first)[:SHOWN]:
            print(f"    {have:3}/{n:<3}  {name}")
        if len(group) > SHOWN:
            print(f"    … and {len(group) - SHOWN} more")

    if out is not None:
        write_tsv(
            out,
            [
                {"identified": have, "files": n, "album": name}
                for name, (n, have) in sorted(found.items(), key=least_covered)
                if have < n
            ],
        )
        print(f"\n  -> {out}")


def register(app: typer.Typer) -> None:
    """Attach the identity audit."""

    @app.command("identity")
    def identity_cmd(
        root: Path = typer.Argument(Path("~/MusicLibrary"), help="library root to audit"),
        out: Path = typer.Option(None, help="also write the full list as TSV"),
    ) -> None:
        """Which albums carry no MusicBrainz id. Changes nothing."""
        here = root.expanduser()
        report(coverage(here), out)
