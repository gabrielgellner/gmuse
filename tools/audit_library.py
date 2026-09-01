#!/usr/bin/env python3
"""Audit a music library's tags and naming against the conventions.

Reads only. Follows the pattern rsgain and bliss already set — an external tool
analyses, gmuse reads the result — because the design brief puts tag editing
out of scope.
"""

from __future__ import annotations

import argparse
import re
import unicodedata
from collections import Counter
from dataclasses import dataclass, field
from pathlib import Path
from typing import Final

from genre.common import tags_of, walk

CORE: Final = ("artist", "album", "album_artist", "title", "track", "date", "genre")
#: Illegal on Windows, FAT and SMB — the library's cross-platform rule.
ILLEGAL: Final = frozenset('<>:"/\\|?*')
#: `NN Title.ext`, or `D-NN Title.ext` on a multi-disc release.
NUMBERED: Final = re.compile(r"^(?:(\d+)-)?(\d+) (.+)\.[^.]+$")


@dataclass
class Findings:
    """What one pass over a library turned up."""

    files: int = 0
    containers: Counter[str] = field(default_factory=Counter)
    tag_names: Counter[str] = field(default_factory=Counter)
    genres: Counter[str] = field(default_factory=Counter)
    missing: Counter[str] = field(default_factory=Counter)
    no_replaygain: list[str] = field(default_factory=list)
    illegal_chars: list[str] = field(default_factory=list)
    not_nfc: list[str] = field(default_factory=list)
    number_mismatch: list[str] = field(default_factory=list)
    has_bpm: int = 0
    has_key: int = 0


def track_number(value: str) -> str:
    """Reduce a track tag to its number, dropping any `n/total` suffix and zero padding."""
    return value.split("/", maxsplit=1)[0].lstrip("0") or "0"


def check_name(path: Path, relative: str, out: Findings) -> None:
    """Check a file's *name* — encoding form and cross-platform legality."""
    if unicodedata.normalize("NFC", relative) != relative:
        out.not_nfc.append(relative)
    if any(char in ILLEGAL for char in path.name):
        out.illegal_chars.append(relative)


def check_tags(path: Path, relative: str, tags: dict[str, str], out: Findings) -> None:
    """Check a file's *tags* — what is present, and what the numbering agrees with."""
    for name in tags:
        out.tag_names[name] += 1
    for name in CORE:
        if not tags.get(name, "").strip():
            out.missing[name] += 1
    if genre := tags.get("genre", "").strip():
        out.genres[genre] += 1
    if not any(k.startswith("replaygain") or "r128" in k for k in tags):
        out.no_replaygain.append(relative)
    if any(k in tags for k in ("bpm", "tbpm")):
        out.has_bpm += 1
    if any("key" in k for k in tags):
        out.has_key += 1

    match = NUMBERED.match(path.name)
    if (
        match
        and tags.get("title")
        and (tagged := track_number(tags.get("track", "")))
        and tagged != (match.group(2).lstrip("0") or "0")
    ):
        out.number_mismatch.append(f"{relative}  (tag track={tags.get('track')})")


def inspect(path: Path, root: Path, out: Findings) -> None:
    """Add one file's findings to `out`."""
    out.files += 1
    relative = str(path.relative_to(root))
    out.containers[path.suffix.lstrip(".").lower()] += 1
    check_name(path, relative, out)
    check_tags(path, relative, tags_of(path), out)


def show(title: str, items: list[str], cap: int = 8) -> None:
    """Print a capped list, saying how much was elided rather than truncating silently."""
    print(f"\n{title}: {len(items)}")
    for item in items[:cap]:
        print(f"    {item}")
    if len(items) > cap:
        print(f"    … and {len(items) - cap} more")


def report(root: Path, out: Findings) -> None:
    """Print the findings."""
    print(f"=== {out.files} files under {root} ===")
    print(f"\ncontainer: {dict(out.containers)}")
    print("\nmissing core tags:")
    for name in CORE:
        share = 100 * out.missing[name] // max(out.files, 1)
        print(f"    {name:14} {out.missing[name]:5}  ({share}%)")
    print(f"\ntag names seen ({len(out.tag_names)} distinct), most common:")
    for name, count in out.tag_names.most_common(18):
        print(f"    {name:28} {count}")
    print(f"\ngenre values ({len(out.genres)} distinct):")
    for genre, count in out.genres.most_common(12):
        print(f"    {count:5}  {genre}")
    show("no ReplayGain", out.no_replaygain)
    show("filename number disagrees with track tag", out.number_mismatch)
    show("Windows-illegal characters in filename", out.illegal_chars)
    show("filename not NFC", out.not_nfc)
    print(f"\nBPM tagged: {out.has_bpm}   key tagged: {out.has_key}")


def main() -> None:
    """Audit the library named on the command line."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", nargs="?", default="~/MusicLibrary", help="library root to audit")
    args = parser.parse_args()

    root = Path(args.root).expanduser()
    out = Findings()
    for path in walk(root):
        inspect(path, root, out)
    report(root, out)


if __name__ == "__main__":
    main()
